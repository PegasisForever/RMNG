//! VA-API H.264 encoder: import a received dmabuf (DMA_DRM) into an appsrc and
//! encode to Annex-B H.264 via `vapostproc → vah264enc` (the Phase-0 R4 path, in
//! Rust). One encoder per monitor of the selected clone.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

pub struct Encoder {
    appsrc: AppSrc,
    pipeline: gst::Pipeline,
    cur: Mutex<Option<(u32, u64, u32, u32)>>, // fourcc, modifier, w, h — caps gate
}

/// DRM fourcc (e.g. 0x34325241) → "AR24".
fn fourcc_str(fourcc: u32) -> String {
    String::from_utf8_lossy(&fourcc.to_le_bytes()).trim_end_matches('\0').to_string()
}

impl Encoder {
    /// `on_au(annexb, is_idr)` is called from a GStreamer thread per access unit.
    pub fn new<F: FnMut(Vec<u8>, bool) + Send + 'static>(mut on_au: F) -> Result<Self> {
        let desc = "appsrc name=src is-live=true format=time do-timestamp=true ! \
             vapostproc ! video/x-raw(memory:VAMemory),format=NV12 ! \
             vah264enc name=enc aud=true b-frames=0 ref-frames=1 key-int-max=30 \
               rate-control=cqp qpi=23 qpp=25 target-usage=7 ! \
             video/x-h264,profile=constrained-baseline ! \
             h264parse config-interval=-1 ! \
             video/x-h264,stream-format=byte-stream,alignment=au ! \
             appsink name=out emit-signals=true max-buffers=4 sync=false";
        let pipeline =
            gst::parse::launch(desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow!("not a pipeline"))?;
        let appsrc =
            pipeline.by_name("src").context("appsrc")?.downcast::<AppSrc>().map_err(|_| anyhow!("not appsrc"))?;
        let appsink =
            pipeline.by_name("out").context("appsink")?.downcast::<AppSink>().map_err(|_| anyhow!("not appsink"))?;

        appsink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(buf) = sample.buffer() {
                        let idr = !buf.flags().contains(gst::BufferFlags::DELTA_UNIT);
                        if let Ok(map) = buf.map_readable() {
                            on_au(map.as_slice().to_vec(), idr);
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline.set_state(gst::State::Playing).context("encoder PLAYING")?;
        Ok(Self { appsrc, pipeline, cur: Mutex::new(None) })
    }

    /// Force the next encoded frame to be an IDR (keyframe) — call on viewer
    /// connect and on selection switch so the picture appears within one frame
    /// instead of waiting up to `key-int-max`. The upstream force-key-unit event
    /// travels from the sink back to `vah264enc`.
    pub fn force_idr(&self) {
        let ev = gstreamer_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        self.pipeline.send_event(ev);
    }

    /// Push one captured dmabuf frame. `fd` is consumed (the GstMemory owns it).
    pub fn push(&self, fd: OwnedFd, fourcc: u32, modifier: u64, w: u32, h: u32) -> Result<()> {
        {
            let mut cur = self.cur.lock().unwrap();
            if *cur != Some((fourcc, modifier, w, h)) {
                // GStreamer's drm-format modifier is `0x` + 16 zero-padded hex
                // digits; a non-padded value (`{:#x}`) is a different *string* and
                // fails caps matching against vapostproc's advertised list.
                let drm = format!("{}:{:#018x}", fourcc_str(fourcc), modifier);
                let caps = gst::Caps::builder("video/x-raw")
                    .features(["memory:DMABuf"])
                    .field("format", "DMA_DRM")
                    .field("drm-format", drm.as_str())
                    .field("width", w as i32)
                    .field("height", h as i32)
                    .build();
                self.appsrc.set_caps(Some(&caps));
                *cur = Some((fourcc, modifier, w, h));
            }
        }
        // Size of the underlying dmabuf (lseek SEEK_END is the canonical query).
        let raw = fd.as_raw_fd();
        let size = nix::unistd::lseek(raw, 0, nix::unistd::Whence::SeekEnd).context("lseek dmabuf")? as usize;
        let allocator = gstreamer_allocators::DmaBufAllocator::new();
        // SAFETY: `fd` is a unique owned dmabuf fd; the GstMemory takes ownership.
        let mem = unsafe { allocator.alloc(fd, size) }.map_err(|e| anyhow!("dmabuf alloc: {e}"))?;
        let mut buffer = gst::Buffer::new();
        buffer.get_mut().unwrap().append_memory(mem);
        self.appsrc.push_buffer(buffer).map_err(|e| anyhow!("push_buffer: {e:?}"))?;
        Ok(())
    }
}
