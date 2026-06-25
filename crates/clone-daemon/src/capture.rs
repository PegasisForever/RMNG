//! dmabuf capture via GStreamer: `pipewiresrc path=<node> ! DMABuf ! appsink`.
//! Each new sample yields a zero-copy dmabuf — we extract its fd(s), plane layout,
//! and DRM fourcc/modifier and hand them to a callback (the shipper). The fd is
//! `dup()`d so it outlives the recycled GstBuffer.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;

use crate::mutter::VirtualMonitor;

/// One captured frame: dmabuf fd(s) + everything needed to import it elsewhere.
/// (The monitor is identified by the capture pipeline, one per monitor — the shipper
/// already knows it, so it isn't carried on the frame.)
pub struct CapturedFrame {
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    /// (offset, stride) per plane.
    pub planes: Vec<(u32, u32)>,
    /// dup'd dmabuf fds, in plane order (one fd if all planes share a buffer).
    pub fds: Vec<OwnedFd>,
}

/// Build + start a capture pipeline for one monitor. `on_frame` is called from a
/// GStreamer streaming thread for every captured dmabuf. Returns the running
/// pipeline (keep it alive; drop to stop).
pub fn start_capture<F>(mon: &VirtualMonitor, mut on_frame: F) -> Result<gst::Pipeline>
where
    F: FnMut(CapturedFrame) + Send + 'static,
{
    // Damage-driven; a leaky 1-deep queue keeps only the freshest frame.
    // A non-VA consumer (appsink) advertises ANY caps, so pipewiresrc has no
    // concrete drm-format to offer the node → "No supported formats found"
    // (Phase-0 R2: VA elements enumerate modifiers, a hand-rolled sink must pin
    // one). Pin the format; default = the W6800's AR24 tiled modifier, overridable
    // for other GPUs via RMNG_DRM_FORMAT (e.g. "AR24:0x0200000020801b03").
    let drm_format = std::env::var("RMNG_DRM_FORMAT")
        .unwrap_or_else(|_| "AR24:0x0200000020801b03".to_string());
    let desc = format!(
        "pipewiresrc path={node} do-timestamp=true keepalive-time=1000 ! \
         video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format={drm} ! \
         queue max-size-buffers=1 leaky=downstream ! \
         appsink name=sink emit-signals=true max-buffers=1 drop=true sync=false",
        node = mon.node_id,
        drm = drm_format
    );
    let pipeline = gst::parse::launch(&desc)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("capture pipeline is not a Pipeline"))?;
    let sink = pipeline
        .by_name("sink")
        .context("appsink 'sink' missing")?
        .downcast::<AppSink>()
        .map_err(|_| anyhow!("'sink' is not an AppSink"))?;

    sink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                match extract(&sample) {
                    Ok(frame) => on_frame(frame),
                    Err(e) => tracing::warn!("capture extract failed: {e}"),
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    pipeline.set_state(gst::State::Playing).context("capture pipeline PLAYING")?;
    Ok(pipeline)
}

fn extract(sample: &gst::Sample) -> Result<CapturedFrame> {
    let buffer = sample.buffer().context("sample has no buffer")?;
    let caps = sample.caps().context("sample has no caps")?;

    // DMA_DRM caps carry the DRM fourcc + modifier; VideoInfoDmaDrm derefs to the
    // underlying VideoInfo for plane offsets/strides.
    let drm = gstreamer_video::VideoInfoDmaDrm::from_caps(caps)
        .map_err(|_| anyhow!("caps are not DMA_DRM: {caps}"))?;
    // The per-plane offset/stride live in the buffer's VideoMeta for DMA_DRM
    // (the DMA_DRM VideoInfo itself is opaque, n_planes=0). Fall back to a single
    // tightly-packed plane if no meta is attached.
    let planes: Vec<(u32, u32)> = match buffer.meta::<gstreamer_video::VideoMeta>() {
        Some(vm) => {
            let n = vm.n_planes() as usize;
            (0..n).map(|i| (vm.offset()[i] as u32, vm.stride()[i] as u32)).collect()
        }
        None => vec![(0, drm.width() * 4)],
    };

    // Collect a dup'd fd per memory (Mutter recycles the original buffer).
    let mut fds: Vec<OwnedFd> = Vec::new();
    for i in 0..buffer.n_memory() {
        let mem = buffer.peek_memory(i);
        let dmabuf = mem
            .downcast_memory_ref::<gstreamer_allocators::DmaBufMemory>()
            .ok_or_else(|| anyhow!("memory {i} is not a dmabuf"))?;
        let raw: RawFd = dmabuf.fd();
        // dup() the borrowed fd; we own the new fd.
        let dup = nix::unistd::dup(raw).context("dup dmabuf fd")?;
        // SAFETY: `dup` is a fresh fd we exclusively own.
        fds.push(unsafe { OwnedFd::from_raw_fd(dup) });
    }

    Ok(CapturedFrame {
        fourcc: drm.fourcc(),
        modifier: drm.modifier(),
        width: drm.width(),
        height: drm.height(),
        planes,
        fds,
    })
}
