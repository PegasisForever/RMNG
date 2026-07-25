//! On-demand screenshot: import a clone's latest dmabuf and encode it to JPEG via
//! `vapostproc → videoconvert → jpegenc` (the clone-local desktop MCP `screenshot` tool).
//! Infrequent + request-driven, so a one-shot pipeline per call is fine.
//!
//! JPEG (not PNG): a full-desktop PNG is multi-MB, and the MCP client caches every
//! returned screenshot to a temp file — those pile up fast. A high-quality JPEG is
//! ~8-10× smaller with no loss the vision model cares about.

use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

use crate::encode::meta_layout;

fn fourcc_str(fourcc: u32) -> String {
    String::from_utf8_lossy(&fourcc.to_le_bytes()).trim_end_matches('\0').to_string()
}

/// JPEG quality for on-demand screenshots (0-100). 90 keeps small UI text crisp
/// for the vision model while staying far smaller than a lossless PNG.
const JPEG_QUALITY: i32 = 90;

/// Encode one captured dmabuf frame to JPEG bytes. `fd` is consumed. `planes` is the
/// daemon-reported per-plane (offset, stride) of the dmabuf.
///
/// `w`/`h` are the frame's **native** size (appsrc caps + `VideoMeta`); `target_w`/`target_h`
/// are the size the JPEG comes out at — `vapostproc` scales on-GPU, so the caller gets the
/// virtual-space image without a host-side decode/resize round-trip. Pass the native size for
/// both to disable scaling. Both targets must be **even** (4:2:0 chroma) — see `round_even`
/// in the daemon's `mcp` module, which is the sole caller.
// The dmabuf's identity (fd + fourcc + modifier + size + plane layout) is irreducibly wide;
// bundling it into a struct for one call site would obscure more than it saves.
#[allow(clippy::too_many_arguments)]
pub fn screenshot_jpeg(
    fd: OwnedFd,
    fourcc: u32,
    modifier: u64,
    w: u32,
    h: u32,
    planes: &[wire::socket::PlaneLayout],
    target_w: u32,
    target_h: u32,
) -> Result<Vec<u8>> {
    // The caps right after `vapostproc` are what makes the scale happen on the GPU (mirrors
    // the encoder's `vapostproc ! video/x-raw(memory:VAMemory)` precedent). Plain `video/x-raw`
    // — no memory feature — so the VA surface lands back in system memory for
    // `videoconvert`+`jpegenc`; adding `videoscale` here would pull the resize onto the CPU.
    let desc = format!(
        "appsrc name=src ! vapostproc ! \
         video/x-raw,width={target_w},height={target_h},pixel-aspect-ratio=1/1 ! \
         videoconvert ! jpegenc quality={JPEG_QUALITY} ! \
         appsink name=out max-buffers=1 sync=false"
    );
    let pipeline =
        gst::parse::launch(&desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow!("not a pipeline"))?;
    let appsrc =
        pipeline.by_name("src").context("appsrc")?.downcast::<AppSrc>().map_err(|_| anyhow!("not appsrc"))?;
    let appsink =
        pipeline.by_name("out").context("appsink")?.downcast::<AppSink>().map_err(|_| anyhow!("not appsink"))?;

    let drm = format!("{}:{:#018x}", fourcc_str(fourcc), modifier);
    appsrc.set_caps(Some(
        &gst::Caps::builder("video/x-raw")
            .features(["memory:DMABuf"])
            .field("format", "DMA_DRM")
            .field("drm-format", drm.as_str())
            .field("width", w as i32)
            .field("height", h as i32)
            .build(),
    ));

    pipeline.set_state(gst::State::Playing).context("screenshot pipeline PLAYING")?;

    // Wrap the dmabuf, push it, then EOS so jpegenc emits the single frame.
    let raw = fd.as_raw_fd();
    let size = nix::unistd::lseek(raw, 0, nix::unistd::Whence::SeekEnd).context("lseek")? as usize;
    let allocator = gstreamer_allocators::DmaBufAllocator::new();
    // SAFETY: unique owned dmabuf fd; the GstMemory takes ownership.
    let mem = unsafe { allocator.alloc(fd, size) }.map_err(|e| anyhow!("dmabuf alloc: {e}"))?;
    let mut buffer = gst::Buffer::new();
    {
        let b = buffer.get_mut().unwrap();
        b.append_memory(mem);
        // Attach the real plane layout: the GPU pads pitches for widths whose pitch isn't
        // 16-aligned (real stride ≠ width·4), and importing the dmabuf with a fabricated
        // width·4 stride reads the frame skewed / rejects the import outright, so the
        // screenshot fails. Mirrors the encoder's VideoMeta fix (`Encoder::push`).
        let vfmt = match fourcc_str(fourcc).as_str() {
            "AB24" | "XB24" => gstreamer_video::VideoFormat::Rgba,
            _ => gstreamer_video::VideoFormat::Bgra, // AR24/XR24 (ARGB/xRGB) and default
        };
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
    appsrc.push_buffer(buffer).map_err(|e| anyhow!("push_buffer: {e:?}"))?;
    let _ = appsrc.end_of_stream();

    let sample = appsink
        .try_pull_sample(gst::ClockTime::from_seconds(5))
        .ok_or_else(|| anyhow!("screenshot timed out"))?;
    let jpeg = sample
        .buffer()
        .and_then(|b| b.map_readable().ok())
        .map(|m| m.as_slice().to_vec())
        .ok_or_else(|| anyhow!("no JPEG buffer"))?;

    let _ = pipeline.set_state(gst::State::Null);
    // Brief settle so the VA surfaces release cleanly between one-shot pipelines.
    std::thread::sleep(Duration::from_millis(1));
    Ok(jpeg)
}
