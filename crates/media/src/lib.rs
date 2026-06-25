//! Media plane (Phase 4): dmabuf ingest → VA-API H.264 (viewer) + input routing.
//! Developed/tested on the AMD W6800 box.

pub mod encode;
pub mod screenshot;
pub mod sock;

pub use encode::Encoder;
pub use screenshot::screenshot_png;
pub use sock::{Conn, Listener};

/// Initialize GStreamer (call once before constructing an [`Encoder`]).
pub fn init() -> anyhow::Result<()> {
    gstreamer::init()?;
    Ok(())
}
