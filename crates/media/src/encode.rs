//! VA-API H.264 encoder: import a received dmabuf (DMA_DRM) into an appsrc and
//! encode to Annex-B H.264. One encoder per monitor of the selected clone.
//!
//! - [`ChromaMode::Yuv420`] (default): the standard single pipeline
//!   `appsrc → vapostproc → NV12 → vah264enc`, one `W×H` 4:2:0 stream.
//! - [`ChromaMode::Yuv444`]: full chroma via the AVC444 double-height trick, as a **single
//!   zero-copy GPU pipeline**: `appsrc(dmabuf) → glupload → rmngavc444pack →
//!   vapostproc(VAMemory NV12) → vah264enc`. The custom [`crate::glpack`] element packs main+aux
//!   into one stacked `W×2H` NV12 frame and renders it straight into a VA-allocated dmabuf
//!   surface — colour-convert, pack and GL→VA bridge fused into one GPU pass (byte-compatible
//!   with [`wire::avc444`]); only the compressed AU leaves the GPU. The viewer reassembles
//!   4:4:4. Requires the headless-GL env set in [`crate::init`].

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use wire::ChromaMode;

pub struct Encoder {
    /// dmabuf input (both modes feed this).
    appsrc: AppSrc,
    /// The whole encode pipeline (where `vah264enc` lives, so `force_idr` targets it).
    pipeline: gst::Pipeline,
    /// fourcc, modifier, w, h, shm — the input caps gate. `shm` is part of the key because
    /// one encoder is shared per (monitor, size) across clones: selecting a GPU-less clone
    /// after a GPU one must re-set the caps even when the frame geometry is identical.
    cur: Mutex<Option<(u32, u64, u32, u32, bool)>>,
    /// `Some` only when `RMNG_ENC_LATENCY` is set: shared FIFO of push timestamps the appsink pops
    /// to measure per-frame push→AU latency. (This VA encoder doesn't preserve input PTS to its
    /// output AUs, so latency is correlated by push order — valid while frames aren't dropped.)
    lat: Option<Arc<Mutex<VecDeque<Instant>>>>,
}

/// DRM fourcc (e.g. 0x34325241) → "AR24".
fn fourcc_str(fourcc: u32) -> String {
    String::from_utf8_lossy(&fourcc.to_le_bytes()).trim_end_matches('\0').to_string()
}

/// Input-queue bound for the appsrc, shared by both modes. Without it the appsrc queue is
/// unbounded: under any encode slowdown frames pile up and per-frame latency grows without recovery
/// (bufferbloat). Measured on the W6800 at a paced 60 fps push, the unbounded 4:4:4 path reached
/// p99 ≈ 115 ms / a 7–8 frame backlog while the median stayed ≈ 13 ms; under sustained overload it
/// runs to seconds. `leaky-type=downstream` drops the OLDEST queued frame when full, so the encoder
/// always pulls the freshest capture (latest-frame-wins — the right policy for a live desktop; a
/// dropped *raw* frame just skips a visual state, unlike a dropped encoded AU which would corrupt
/// the H.264 reference chain, so the appsink stays lossless). `max-buffers=2` keeps a little
/// pack/encode pipelining slack while bounding the input to ~2 frames. Tighter (`=1`) trims the
/// tail a few ms more but removes that slack. `do-timestamp` is unaffected.
const APPSRC_BOUND: &str = "max-buffers=2 leaky-type=downstream";

/// The colour description of the 4:2:0 stream: `range:matrix:transfer:primaries` =
/// limited range (16..235), BT.709 matrix, transfer and primaries left unset.
///
/// `vah264enc` writes no VUI colour description, so nothing in the bitstream tells the viewer
/// how these samples were made, so the two halves have to agree by construction. Pinning it here
/// removes the guess: without it GStreamer picks a default from the frame size, and a preset
/// small enough to read as SD would flip the matrix under the viewer's feet.
///
/// The transfer stays unset **on purpose**. Naming one (even the true `sRGB`) makes `vapostproc`
/// apply a transfer conversion on top of the matrix, which crushes the dark end: a source 16
/// lands on luma 22 instead of 30. The samples we want are a plain matrix + range change, and
/// the viewer supplies the missing transfer on the way out (`viewer::VIDEO_COLORIMETRY`).
const ENC_COLORIMETRY: &str = "2:3:0:0";

/// `vah264enc → h264parse → appsink` tail, shared by both modes.
///
/// `target-usage=1` is deliberate and **counterintuitive**: on this AMD VCN the usage mapping is
/// effectively inverted — `tu=1` (the "quality" preset) encodes the stacked 2880p Yuv444 frame at
/// ~71fps while `tu=7` (the "speed" preset) manages only ~41fps (measured via `avc444_e2e bench`).
/// `tu=1` is what lets the Yuv444 path keep up with the 60Hz capture; Yuv420 has headroom either way.
const ENC_TAIL: &str = "vah264enc name=enc aud=true b-frames=0 ref-frames=1 key-int-max=30 \
       rate-control=cqp qpi=23 qpp=25 target-usage=1 ! \
     video/x-h264,profile=constrained-baseline ! \
     h264parse config-interval=-1 ! \
     video/x-h264,stream-format=byte-stream,alignment=au ! \
     appsink name=out emit-signals=true max-buffers=4 sync=false";

impl Encoder {
    /// `on_au(annexb, is_idr)` is called from a GStreamer thread per access unit.
    pub fn new<F: FnMut(Vec<u8>, bool) + Send + 'static>(
        chroma: ChromaMode,
        on_au: F,
    ) -> Result<Self> {
        match chroma {
            ChromaMode::Yuv420 => Self::new_yuv420(on_au),
            ChromaMode::Yuv444 => Self::new_yuv444(on_au),
        }
    }

    fn new_yuv420<F: FnMut(Vec<u8>, bool) + Send + 'static>(on_au: F) -> Result<Self> {
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=true {APPSRC_BOUND} ! \
             vapostproc ! video/x-raw(memory:VAMemory),format=NV12,colorimetry={ENC_COLORIMETRY} ! \
             {ENC_TAIL}"
        );
        let pipeline = launch_pipeline(&desc)?;
        let appsrc = by_name_appsrc(&pipeline, "src")?;
        let lat = lat_fifo();
        attach_au_sink(&pipeline, on_au, lat.clone())?;
        pipeline.set_state(gst::State::Playing).context("encoder PLAYING")?;
        Ok(Self { appsrc, pipeline, cur: Mutex::new(None), lat })
    }

    fn new_yuv444<F: FnMut(Vec<u8>, bool) + Send + 'static>(on_au: F) -> Result<Self> {
        // Register our GPU packer before referencing it by name.
        crate::glpack::register()?;
        // Single zero-copy GPU pipeline: `glupload` imports the capture dmabuf as a GL texture,
        // then `rmngavc444pack` (colour-convert + AVC444 pack + GL→VA bridge fused into one pass)
        // renders the packed stacked-NV12 planes straight into a VA-allocated dmabuf surface —
        // GstGL's gldownload can't export AMD's tiled GL textures in a layout VA accepts, so we
        // invert ownership (VA allocates, GL writes in place), which vapostproc/vah264enc consume.
        // No per-frame host transfers. (push() attaches a VideoMeta so glupload can EGL-import the
        // bare compositor dmabuf zero-copy — without it glupload derives 0 planes and CPU-mmaps.)
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=true {APPSRC_BOUND} ! \
             glupload ! rmngavc444pack ! \
             vapostproc ! video/x-raw(memory:VAMemory),format=NV12 ! {ENC_TAIL}"
        );
        let pipeline = launch_pipeline(&desc)?;
        let appsrc = by_name_appsrc(&pipeline, "src")?;
        let lat = lat_fifo();
        attach_au_sink(&pipeline, on_au, lat.clone())?;
        pipeline.set_state(gst::State::Playing).context("encoder PLAYING")?;
        Ok(Self { appsrc, pipeline, cur: Mutex::new(None), lat })
    }

    /// Force the next encoded frame to be an IDR (keyframe). The event reaches `vah264enc`
    /// in the single pipeline.
    pub fn force_idr(&self) {
        let ev = gstreamer_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        self.pipeline.send_event(ev);
    }

    /// Push one captured frame. `fd` is consumed (the GstMemory owns it).
    /// `planes` is the daemon-reported per-plane (offset, stride).
    ///
    /// `shm` marks `fd` as a memfd of system memory rather than a dmabuf, which is what a
    /// clone with no GPU captures: its compositor runs surfaceless and its screencast
    /// source offers shm buffers only. The pipeline is the same either way — the frame
    /// enters as plain `video/x-raw` and `vapostproc` uploads it to a VA surface, so the
    /// encode still runs on the server's GPU. What is lost is the zero-copy import: the
    /// upload reads `width·height·4` bytes per frame across the bus.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &self,
        fd: OwnedFd,
        fourcc: u32,
        modifier: u64,
        w: u32,
        h: u32,
        planes: &[wire::socket::PlaneLayout],
        shm: bool,
    ) -> Result<()> {
        if shm {
            return self.push_shm(fd, fourcc, w, h, planes);
        }
        {
            let mut cur = self.cur.lock().unwrap();
            if *cur != Some((fourcc, modifier, w, h, false)) {
                // GStreamer's drm-format modifier is `0x` + 16 zero-padded hex digits; a
                // non-padded value (`{:#x}`) is a different *string* and fails caps matching.
                let drm = format!("{}:{:#018x}", fourcc_str(fourcc), modifier);
                let caps = gst::Caps::builder("video/x-raw")
                    .features(["memory:DMABuf"])
                    .field("format", "DMA_DRM")
                    .field("drm-format", drm.as_str())
                    .field("width", w as i32)
                    .field("height", h as i32)
                    .build();
                self.appsrc.set_caps(Some(&caps));
                *cur = Some((fourcc, modifier, w, h, false));
            }
        }
        // Size of the underlying dmabuf (lseek SEEK_END is the canonical query).
        let raw = fd.as_raw_fd();
        let size = nix::unistd::lseek(raw, 0, nix::unistd::Whence::SeekEnd).context("lseek dmabuf")? as usize;
        let allocator = gstreamer_allocators::DmaBufAllocator::new();
        // SAFETY: `fd` is a unique owned dmabuf fd; the GstMemory takes ownership.
        let mem = unsafe { allocator.alloc(fd, size) }.map_err(|e| anyhow!("dmabuf alloc: {e}"))?;
        let mut buffer = gst::Buffer::new();
        {
            let b = buffer.get_mut().unwrap();
            b.append_memory(mem);
            // Attach plane metadata so GL import (glupload, Yuv444 path) works: the bare
            // dmabuf buffer carries none, so glupload derives 0 planes and falls back to a
            // failing CPU mmap. The layout must be the *real* one the compositor allocated:
            // the GPU pads pitches (width 1440 → a 256-byte-aligned pitch, not 1440·4), and
            // an EGL import with a wrong pitch is rejected, killing the stream. VA (Yuv420
            // path) ignores the meta and derives the layout itself, so this is safe for
            // both modes.
            let vfmt = video_format_for(fourcc);
            let (offsets, strides) = meta_layout(w, planes);
            let _ = gstreamer_video::VideoMeta::add_full(
                b,
                gstreamer_video::VideoFrameFlags::empty(),
                vfmt,
                w,
                h,
                &offsets,
                &strides,
            );
        }
        // For latency measurement, record the push instant just before handing the frame off; the
        // appsink pops it per AU. Enqueue only on a successful push so the FIFO stays aligned.
        let t0 = self.lat.as_ref().map(|_| Instant::now());
        self.appsrc.push_buffer(buffer).map_err(|e| anyhow!("push_buffer: {e:?}"))?;
        if let (Some(fifo), Some(t0)) = (&self.lat, t0) {
            fifo.lock().unwrap().push_back(t0);
        }
        Ok(())
    }

    /// Push one shm frame: the memfd is wrapped as fd-backed `GstMemory` (mapped on demand,
    /// never copied here) and enters the pipeline as plain `video/x-raw`.
    fn push_shm(
        &self,
        fd: OwnedFd,
        fourcc: u32,
        w: u32,
        h: u32,
        planes: &[wire::socket::PlaneLayout],
    ) -> Result<()> {
        let vfmt = video_format_for(fourcc);
        {
            let mut cur = self.cur.lock().unwrap();
            if *cur != Some((fourcc, 0, w, h, true)) {
                let caps = gst::Caps::builder("video/x-raw")
                    .field("format", vfmt.to_str().as_str())
                    .field("width", w as i32)
                    .field("height", h as i32)
                    .field("framerate", gst::Fraction::new(0, 1))
                    .build();
                self.appsrc.set_caps(Some(&caps));
                *cur = Some((fourcc, 0, w, h, true));
            }
        }
        let buffer = sysmem_buffer(fd, vfmt, w, h, planes)?;
        let t0 = self.lat.as_ref().map(|_| Instant::now());
        self.appsrc.push_buffer(buffer).map_err(|e| anyhow!("push_buffer: {e:?}"))?;
        if let (Some(fifo), Some(t0)) = (&self.lat, t0) {
            fifo.lock().unwrap().push_back(t0);
        }
        Ok(())
    }
}

/// The GStreamer pixel format for a DRM fourcc. Used for the `VideoMeta` on an imported
/// dmabuf and for the caps of a system-memory frame alike.
pub(crate) fn video_format_for(fourcc: u32) -> gstreamer_video::VideoFormat {
    match fourcc_str(fourcc).as_str() {
        "AB24" | "XB24" => gstreamer_video::VideoFormat::Rgba,
        _ => gstreamer_video::VideoFormat::Bgra, // AR24/XR24 (ARGB/xRGB) and default
    }
}

/// Wrap a memfd of raw pixels as a one-memory `GstBuffer` carrying the real plane layout.
/// `fd` is consumed: the `GstMemory` owns it and closes it when the buffer is released.
///
/// The whole fd is wrapped and the `VideoMeta` plane offsets address the frame inside it.
/// The compositor's mapping offset is already folded into those offsets by the daemon, so a
/// frame that sits partway into a pooled memfd still reads correctly.
pub(crate) fn sysmem_buffer(
    fd: OwnedFd,
    vfmt: gstreamer_video::VideoFormat,
    w: u32,
    h: u32,
    planes: &[wire::socket::PlaneLayout],
) -> Result<gst::Buffer> {
    let size = nix::unistd::lseek(fd.as_raw_fd(), 0, nix::unistd::Whence::SeekEnd)
        .context("lseek memfd")? as usize;
    let allocator = gstreamer_allocators::FdAllocator::new();
    // MAP_PRIVATE: we only ever read these pixels, and a shared writable mapping needs the fd
    // itself to be writable — which a compositor's screencast memfd need not be.
    // SAFETY: `fd` is a unique owned memfd and the GstMemory takes ownership of it — hence
    // `into_raw_fd`, since the allocator closes it itself unless DONT_CLOSE is set.
    let mem = unsafe {
        allocator.alloc(
            std::os::fd::IntoRawFd::into_raw_fd(fd),
            size,
            gstreamer_allocators::FdMemoryFlags::MAP_PRIVATE,
        )
    }
    .map_err(|e| anyhow!("fd alloc: {e}"))?;
    let mut buffer = gst::Buffer::new();
    {
        let b = buffer.get_mut().unwrap();
        b.append_memory(mem);
        let (offsets, strides) = meta_layout(w, planes);
        let _ = gstreamer_video::VideoMeta::add_full(
            b,
            gstreamer_video::VideoFrameFlags::empty(),
            vfmt,
            w,
            h,
            &offsets,
            &strides,
        );
    }
    Ok(buffer)
}

/// VideoMeta plane layout for a pushed dmabuf: the daemon-reported per-plane
/// (offset, stride) when present, else the packed single-plane fallback
/// (stride = width·4) for callers with no plane info.
pub(crate) fn meta_layout(w: u32, planes: &[wire::socket::PlaneLayout]) -> (Vec<usize>, Vec<i32>) {
    if planes.is_empty() {
        (vec![0], vec![(w * 4) as i32])
    } else {
        (
            planes.iter().map(|p| p.offset as usize).collect(),
            planes.iter().map(|p| p.stride as i32).collect(),
        )
    }
}

/// `Some(empty FIFO)` when `RMNG_ENC_LATENCY` is set, else `None` (zero overhead in production).
fn lat_fifo() -> Option<Arc<Mutex<VecDeque<Instant>>>> {
    std::env::var_os("RMNG_ENC_LATENCY").map(|_| Arc::new(Mutex::new(VecDeque::new())))
}

fn launch_pipeline(desc: &str) -> Result<gst::Pipeline> {
    let pipeline: gst::Pipeline =
        gst::parse::launch(desc)?.downcast().map_err(|_| anyhow!("not a pipeline"))?;
    log_bus_errors(&pipeline);
    Ok(pipeline)
}

/// Log the pipeline's own ERROR/WARNING messages.
///
/// Without this an element that rejects a frame fails **silently**: `push_buffer` returns OK
/// (appsrc queued it), the element posts an error on the bus, nobody reads the bus, and the
/// only symptom is a viewer that stays black. A sync handler needs no main loop and no
/// thread: GStreamer calls it on whichever thread posted the message.
fn log_bus_errors(pipeline: &gst::Pipeline) {
    let Some(bus) = pipeline.bus() else { return };
    bus.set_sync_handler(|_, msg| {
        match msg.view() {
            gst::MessageView::Error(e) => tracing::error!(
                "encode pipeline error from {}: {} ({})",
                e.src().map(|s| s.path_string().to_string()).unwrap_or_default(),
                e.error(),
                e.debug().unwrap_or_default()
            ),
            gst::MessageView::Warning(w) => tracing::warn!(
                "encode pipeline warning from {}: {} ({})",
                w.src().map(|s| s.path_string().to_string()).unwrap_or_default(),
                w.error(),
                w.debug().unwrap_or_default()
            ),
            _ => {}
        }
        gst::BusSyncReply::Drop
    });
}

fn by_name_appsrc(p: &gst::Pipeline, name: &str) -> Result<AppSrc> {
    p.by_name(name).with_context(|| format!("appsrc {name}"))?.downcast().map_err(|_| anyhow!("not appsrc"))
}

/// Wire the `out` appsink to call `on_au(annexb, is_idr)` per access unit.
///
/// When `lat` is `Some` (`RMNG_ENC_LATENCY` set), also measures per-frame **push→AU latency** (the
/// encode stage's own contribution, queueing included — so it exposes bufferbloat) by popping the
/// push instant `push()` enqueued for this frame, and logs p50/p99/max every 120 AUs. Purely
/// additive: no pipeline element or property changes.
fn attach_au_sink<F: FnMut(Vec<u8>, bool) + Send + 'static>(
    p: &gst::Pipeline,
    mut on_au: F,
    lat: Option<Arc<Mutex<VecDeque<Instant>>>>,
) -> Result<()> {
    let appsink: AppSink =
        p.by_name("out").context("appsink out")?.downcast().map_err(|_| anyhow!("not appsink"))?;
    let mut samples: Vec<f64> = Vec::new();
    appsink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(buf) = sample.buffer() {
                    let idr = !buf.flags().contains(gst::BufferFlags::DELTA_UNIT);
                    if let Some(fifo) = &lat {
                        if let Some(t0) = fifo.lock().unwrap().pop_front() {
                            samples.push(t0.elapsed().as_secs_f64() * 1000.0);
                            if samples.len() >= 120 {
                                report_latency("enc push→AU", &mut samples);
                            }
                        }
                    }
                    if let Ok(map) = buf.map_readable() {
                        on_au(map.as_slice().to_vec(), idr);
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(())
}

/// Log p50/p99/max/mean of the collected latency samples (ms) and clear them.
fn report_latency(tag: &str, samples: &mut Vec<f64>) {
    if samples.is_empty() {
        return;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    let pct = |p: f64| samples[(((n - 1) as f64 * p).round() as usize).min(n - 1)];
    let mean = samples.iter().sum::<f64>() / n as f64;
    tracing::info!(
        "[{tag}] n={n} p50={:.1}ms p99={:.1}ms max={:.1}ms mean={:.1}ms",
        pct(0.50),
        pct(0.99),
        samples[n - 1],
        mean
    );
    samples.clear();
}

#[cfg(test)]
mod tests {
    use super::meta_layout;
    use wire::socket::PlaneLayout;

    #[test]
    fn meta_layout_uses_real_plane_strides() {
        // Width 1440: the GPU pads the pitch to 6144 (256-byte aligned) — the meta
        // must carry that real pitch, not the packed 1440·4 = 5760.
        let planes = [PlaneLayout { offset: 0, stride: 6144 }];
        let (offsets, strides) = meta_layout(1440, &planes);
        assert_eq!(offsets, vec![0]);
        assert_eq!(strides, vec![6144]);
    }

    #[test]
    fn meta_layout_passes_through_multiplane() {
        let planes =
            [PlaneLayout { offset: 0, stride: 8192 }, PlaneLayout { offset: 1024, stride: 4096 }];
        let (offsets, strides) = meta_layout(1920, &planes);
        assert_eq!(offsets, vec![0, 1024]);
        assert_eq!(strides, vec![8192, 4096]);
    }

    #[test]
    fn meta_layout_falls_back_to_packed_when_no_planes() {
        let (offsets, strides) = meta_layout(1920, &[]);
        assert_eq!(offsets, vec![0]);
        assert_eq!(strides, vec![1920 * 4]);
    }
}
