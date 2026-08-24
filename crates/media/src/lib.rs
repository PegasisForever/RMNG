//! Media plane (Phase 4): dmabuf ingest → VA-API H.264 (viewer) + input routing.
//! Developed/tested on the AMD W6800 box.

pub mod encode;
pub mod glpack;
pub mod screenshot;
pub mod sock;

pub use encode::Encoder;
pub use screenshot::{screenshot_jpeg, screenshot_jpeg_shm};
pub use sock::{Conn, Listener};

/// Initialize GStreamer (call once before constructing an [`Encoder`]).
///
/// On the **headless control-server** (no display; the GPU is exposed only as a
/// `renderD128` render node, no `card*` node) GstGL's GBM backend can't autodetect a DRM
/// device, so the AVC444 (`ChromaMode::Yuv444`) GL pack pipeline fails with
/// `EGL_NOT_INITIALIZED`. Point GBM at the render node explicitly — only if unset, so an
/// operator override still wins. No card node needed. This is a no-op for the 4:2:0 path
/// (no GL elements) and is applied only here — the viewer has a real display and never
/// calls `media::init`.
pub fn init() -> anyhow::Result<()> {
    set_if_unset("GST_GL_PLATFORM", "egl");
    set_if_unset("GST_GL_WINDOW", "gbm");
    set_if_unset("GST_GL_GBM_DRM_DEVICE", "/dev/dri/renderD128");
    gstreamer::init()?;
    Ok(())
}

fn set_if_unset(key: &str, val: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: called once at startup, before any GStreamer/GL thread is spawned.
        unsafe { std::env::set_var(key, val) };
    }
}
