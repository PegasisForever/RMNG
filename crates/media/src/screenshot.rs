//! On-demand screenshot: import a clone's latest dmabuf and encode it to PNG via
//! `vapostproc → videoconvert → pngenc` (the desktop-MCP `screenshot` tool, port 3/4).
//! Infrequent + request-driven, so a one-shot pipeline per call is fine.

use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

fn fourcc_str(fourcc: u32) -> String {
    String::from_utf8_lossy(&fourcc.to_le_bytes()).trim_end_matches('\0').to_string()
}

/// Encode one captured dmabuf frame to PNG bytes. `fd` is consumed.
pub fn screenshot_png(fd: OwnedFd, fourcc: u32, modifier: u64, w: u32, h: u32) -> Result<Vec<u8>> {
    let desc = "appsrc name=src ! vapostproc ! videoconvert ! pngenc ! \
         appsink name=out max-buffers=1 sync=false";
    let pipeline =
        gst::parse::launch(desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow!("not a pipeline"))?;
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

    // Wrap the dmabuf, push it, then EOS so pngenc emits the single frame.
    let raw = fd.as_raw_fd();
    let size = nix::unistd::lseek(raw, 0, nix::unistd::Whence::SeekEnd).context("lseek")? as usize;
    let allocator = gstreamer_allocators::DmaBufAllocator::new();
    // SAFETY: unique owned dmabuf fd; the GstMemory takes ownership.
    let mem = unsafe { allocator.alloc(fd, size) }.map_err(|e| anyhow!("dmabuf alloc: {e}"))?;
    let mut buffer = gst::Buffer::new();
    buffer.get_mut().unwrap().append_memory(mem);
    appsrc.push_buffer(buffer).map_err(|e| anyhow!("push_buffer: {e:?}"))?;
    let _ = appsrc.end_of_stream();

    let sample = appsink
        .try_pull_sample(gst::ClockTime::from_seconds(5))
        .ok_or_else(|| anyhow!("screenshot timed out"))?;
    let png = sample
        .buffer()
        .and_then(|b| b.map_readable().ok())
        .map(|m| m.as_slice().to_vec())
        .ok_or_else(|| anyhow!("no PNG buffer"))?;

    let _ = pipeline.set_state(gst::State::Null);
    // Brief settle so the VA surfaces release cleanly between one-shot pipelines.
    std::thread::sleep(Duration::from_millis(1));
    Ok(png)
}
