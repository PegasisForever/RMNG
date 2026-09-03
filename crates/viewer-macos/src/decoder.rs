//! VideoToolbox hardware H.264 decode: one [`Decoder`] per monitor, fed AnnexB access units,
//! producing IOSurface-backed `CVPixelBuffer`s (zero-copy to Metal). The GStreamer `vtdec_hw`
//! path of the GTK viewer, re-expressed directly on VideoToolbox.
//!
//! - 4:2:0 (`Yuv420`): a `W×H` NV12 buffer — the displayed image.
//! - 4:4:4 (`Yuv444`): a `W×2H` NV12 buffer carrying the AVC444 stacked main+aux views; the
//!   renderer reconstructs `W×H` 4:4:4 from it (see [`crate::render`]).
//!
//! A `DecodedFrame` hands the pixel buffer to the main (render) thread. `CVPixelBuffer` is a
//! CoreFoundation type that is safe to retain/read across threads, so the newtype's `Send` is
//! sound; we only ever read it on the main thread and drop it there.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo,
};
use objc2_core_video::CVImageBuffer;
use objc2_video_toolbox::{
    VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
};

/// A decoded frame handed to the render thread. Wraps the IOSurface-backed pixel buffer plus the
/// geometry the renderer needs.
pub struct DecodedFrame {
    pub pixel_buffer: CFRetained<CVImageBuffer>,
    /// Displayed image size (for 4:4:4 this is the reconstructed `W×H`, i.e. half the buffer's
    /// height).
    pub width: usize,
    pub height: usize,
    /// The pixel buffer is the double-height AVC444 stacked NV12 needing reconstruction.
    pub yuv444: bool,
}

// SAFETY: `CVPixelBuffer` (a CF type) is safe to retain, read, and release from any thread; we
// move it net-thread → main-thread and only read it there. No `!Send` interior is exposed.
unsafe impl Send for DecodedFrame {}

/// Sink for finished frames: `(monitor_id, frame)`.
pub type FrameSink = Arc<dyn Fn(u32, DecodedFrame) + Send + Sync>;

/// Context reached by the C output callback through its refcon pointer.
struct CallbackCtx {
    monitor_id: u32,
    yuv444: bool,
    sink: FrameSink,
}

pub struct Decoder {
    monitor_id: u32,
    yuv444: bool,
    /// Current parameter sets (SPS then PPS); the session is rebuilt when these change.
    sps: Vec<u8>,
    pps: Vec<u8>,
    format: Option<CFRetained<CMFormatDescription>>,
    session: Option<CFRetained<VTDecompressionSession>>,
    /// Boxed so its address is stable for the whole session lifetime (passed as the callback
    /// refcon). Kept alive here; dropped with the Decoder.
    ctx: Box<CallbackCtx>,
}

impl Decoder {
    pub fn new(monitor_id: u32, yuv444: bool, sink: FrameSink) -> Self {
        Decoder {
            monitor_id,
            yuv444,
            sps: Vec::new(),
            pps: Vec::new(),
            format: None,
            session: None,
            ctx: Box::new(CallbackCtx { monitor_id, yuv444, sink }),
        }
    }

    pub fn yuv444(&self) -> bool {
        self.yuv444
    }

    /// Decode one AnnexB access unit. Updates parameter sets / rebuilds the session as needed,
    /// then submits the VCL NALs as one AVCC sample buffer.
    pub fn decode(&mut self, au: &[u8]) -> Result<()> {
        let nals = split_annexb(au);
        if nals.is_empty() {
            return Ok(());
        }
        // Refresh parameter sets from this AU (IDRs carry SPS/PPS).
        let mut sps = None;
        let mut pps = None;
        for n in &nals {
            match n.first().map(|b| b & 0x1f) {
                Some(7) => sps = Some(n.to_vec()),
                Some(8) => pps = Some(n.to_vec()),
                _ => {}
            }
        }
        let mut changed = false;
        if let Some(s) = sps {
            if s != self.sps {
                self.sps = s;
                changed = true;
            }
        }
        if let Some(p) = pps {
            if p != self.pps {
                self.pps = p;
                changed = true;
            }
        }
        if changed && !self.sps.is_empty() && !self.pps.is_empty() {
            self.build_session()?;
        }
        let Some(session) = self.session.as_ref() else {
            // No parameter sets yet (waiting for the first IDR): drop non-VCL bootstrap.
            return Ok(());
        };

        // AVCC payload: 4-byte big-endian length + NAL, for every non-parameter-set NAL.
        let mut avcc = Vec::with_capacity(au.len());
        for n in &nals {
            match n.first().map(|b| b & 0x1f) {
                Some(7) | Some(8) => continue, // parameter sets live in the format description
                _ => {}
            }
            avcc.extend_from_slice(&(n.len() as u32).to_be_bytes());
            avcc.extend_from_slice(n);
        }
        if avcc.is_empty() {
            return Ok(());
        }
        let block = make_block_buffer(&avcc)?;
        let format = self.format.as_ref().ok_or_else(|| anyhow!("no format description"))?;
        let sample = make_sample_buffer(&block, format, avcc.len())?;

        let ctx_ptr = &*self.ctx as *const CallbackCtx as *mut c_void;
        let mut info = VTDecodeInfoFlags::empty();
        // Synchronous decode: no async flag, so the output callback runs before this returns and
        // frames stay ordered per monitor (H.264 decode order == our display order here).
        let status = unsafe {
            session.decode_frame(&sample, VTDecodeFrameFlags::empty(), ctx_ptr, &mut info)
        };
        if status != 0 {
            bail!("VTDecompressionSessionDecodeFrame failed: {status}");
        }
        Ok(())
    }

    fn build_session(&mut self) -> Result<()> {
        // (Re)build the H.264 format description from the current parameter sets.
        let params: [NonNull<u8>; 2] = [
            NonNull::new(self.sps.as_ptr() as *mut u8).unwrap(),
            NonNull::new(self.pps.as_ptr() as *mut u8).unwrap(),
        ];
        let sizes: [usize; 2] = [self.sps.len(), self.pps.len()];
        let mut fmt: *const CMFormatDescription = std::ptr::null();
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                2,
                NonNull::new(params.as_ptr() as *mut NonNull<u8>).unwrap(),
                NonNull::new(sizes.as_ptr() as *mut usize).unwrap(),
                4,
                NonNull::new(&mut fmt as *mut *const CMFormatDescription).unwrap(),
            )
        };
        if status != 0 || fmt.is_null() {
            bail!("CMVideoFormatDescriptionCreateFromH264ParameterSets failed: {status}");
        }
        // SAFETY: create-rule returns +1; adopt it.
        let format = unsafe { CFRetained::from_raw(NonNull::new(fmt as *mut CMFormatDescription).unwrap()) };

        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: std::ptr::null_mut(),
        };
        let mut session: *mut VTDecompressionSession = std::ptr::null_mut();
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                None, // default destination attributes: VT hands back Metal-compatible NV12
                &callback,
                NonNull::new(&mut session as *mut *mut VTDecompressionSession).unwrap(),
            )
        };
        if status != 0 || session.is_null() {
            bail!("VTDecompressionSessionCreate failed: {status}");
        }
        // SAFETY: create-rule +1; adopt.
        let session = unsafe { CFRetained::from_raw(NonNull::new(session).unwrap()) };
        self.format = Some(format);
        self.session = Some(session);
        tracing::info!("monitor {}: VT decode session (re)built (yuv444={})", self.monitor_id, self.yuv444);
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(session) = self.session.as_ref() {
            unsafe { session.invalidate() };
        }
    }
}

/// The C output callback. `source_ref_con` is our per-decode refcon (the `CallbackCtx`).
unsafe extern "C-unwind" fn output_callback(
    _decompression_output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: i32,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_timestamp: objc2_core_media::CMTime,
    _presentation_duration: objc2_core_media::CMTime,
) {
    if status != 0 || image_buffer.is_null() || source_frame_ref_con.is_null() {
        return;
    }
    // SAFETY: refcon is the stable `&*self.ctx` pointer passed to decode_frame.
    let ctx = unsafe { &*(source_frame_ref_con as *const CallbackCtx) };
    // SAFETY: VT hands us a borrowed +0 image buffer; retain it to keep it past the callback.
    let pb = unsafe { CFRetained::retain(NonNull::new(image_buffer).unwrap()) };
    let (bw, bh) = image_buffer_dims(&pb);
    let (width, height) = if ctx.yuv444 { (bw, bh / 2) } else { (bw, bh) };
    (ctx.sink)(ctx.monitor_id, DecodedFrame { pixel_buffer: pb, width, height, yuv444: ctx.yuv444 });
}

/// `(width, height)` of a pixel buffer, via the CoreVideo C getters.
fn image_buffer_dims(pb: &CVImageBuffer) -> (usize, usize) {
    unsafe extern "C-unwind" {
        fn CVPixelBufferGetWidth(pb: &CVImageBuffer) -> usize;
        fn CVPixelBufferGetHeight(pb: &CVImageBuffer) -> usize;
    }
    unsafe { (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb)) }
}

/// Split an AnnexB buffer into its NAL units (payloads, start codes stripped).
fn split_annexb(buf: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    let n = buf.len();
    // Find the first start code.
    let mut start = find_start_code(buf, 0);
    while let Some((sc_pos, sc_len)) = start {
        let nal_start = sc_pos + sc_len;
        let next = find_start_code(buf, nal_start);
        let nal_end = next.map(|(p, _)| p).unwrap_or(n);
        if nal_end > nal_start {
            nals.push(&buf[nal_start..nal_end]);
        }
        start = next;
        i = nal_end;
    }
    let _ = i;
    nals
}

/// Return `(position, length)` of the next Annex-B start code (`00 00 01` or `00 00 00 01`) at or
/// after `from`.
fn find_start_code(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            return Some((i, 3));
        }
        if i + 4 <= buf.len() && buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 0 && buf[i + 3] == 1 {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// Wrap `data` (copied) in a CMBlockBuffer.
fn make_block_buffer(data: &[u8]) -> Result<CFRetained<CMBlockBuffer>> {
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // Allocate a managed block of the right size, then copy our bytes into it.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            data.len(),
            None, // default allocator: CMBlockBuffer owns the memory
            std::ptr::null(),
            0,
            data.len(),
            objc2_core_media::kCMBlockBufferAssureMemoryNowFlag,
            NonNull::new(&mut block as *mut *mut CMBlockBuffer).unwrap(),
        )
    };
    if status != 0 || block.is_null() {
        bail!("CMBlockBufferCreateWithMemoryBlock failed: {status}");
    }
    let block = unsafe { CFRetained::from_raw(NonNull::new(block).unwrap()) };
    let status = unsafe {
        CMBlockBuffer::replace_data_bytes(
            NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
            &block,
            0,
            data.len(),
        )
    };
    if status != 0 {
        bail!("CMBlockBufferReplaceDataBytes failed: {status}");
    }
    Ok(block)
}

/// Build a ready CMSampleBuffer from an AVCC block buffer + format description.
fn make_sample_buffer(
    block: &CMBlockBuffer,
    format: &CMFormatDescription,
    size: usize,
) -> Result<CFRetained<CMSampleBuffer>> {
    let mut sample: *mut CMSampleBuffer = std::ptr::null_mut();
    let timing = CMSampleTimingInfo {
        duration: unsafe { objc2_core_media::kCMTimeInvalid },
        presentationTimeStamp: unsafe { objc2_core_media::kCMTimeInvalid },
        decodeTimeStamp: unsafe { objc2_core_media::kCMTimeInvalid },
    };
    let sizes = [size];
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(block),
            Some(format),
            1,
            1,
            &timing,
            1,
            sizes.as_ptr(),
            NonNull::new(&mut sample as *mut *mut CMSampleBuffer).unwrap(),
        )
    };
    if status != 0 || sample.is_null() {
        bail!("CMSampleBufferCreateReady failed: {status}");
    }
    Ok(unsafe { CFRetained::from_raw(NonNull::new(sample).unwrap()) })
}

// VTDecodeFrameFlags is a bitflags newtype over u32; the crate exposes the type but we only ever
// pass "no flags" (synchronous, default processing).
#[allow(non_camel_case_types)]
type VTDecodeFrameFlags = objc2_video_toolbox::VTDecodeFrameFlags;

// The one C entry point the CoreMedia crate declares as a free function.
unsafe extern "C-unwind" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: Option<&objc2_core_foundation::CFAllocator>,
        parameter_set_count: usize,
        parameter_set_pointers: NonNull<NonNull<u8>>,
        parameter_set_sizes: NonNull<usize>,
        nal_unit_header_length: i32,
        format_description_out: NonNull<*const CMFormatDescription>,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_split_finds_all_nals_both_start_code_lengths() {
        // 4-byte SC, NAL [0x67 ..], 3-byte SC, NAL [0x68 ..], 3-byte SC, NAL [0x65 ..]
        let buf = [
            0, 0, 0, 1, 0x67, 0x11, 0x22, //
            0, 0, 1, 0x68, 0x33, //
            0, 0, 1, 0x65, 0x44, 0x55,
        ];
        let nals = split_annexb(&buf);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], &[0x67, 0x11, 0x22]);
        assert_eq!(nals[1], &[0x68, 0x33]);
        assert_eq!(nals[2], &[0x65, 0x44, 0x55]);
    }

    #[test]
    fn annexb_split_empty_when_no_start_code() {
        assert!(split_annexb(&[0x67, 0x11]).is_empty());
    }
}
