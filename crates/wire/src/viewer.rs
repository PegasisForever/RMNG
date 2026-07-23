//! The native viewer ⇄ control-server protocol (port 1).
//!
//! Length-prefixed frames over one TCP connection: video/cursor/clipboard out,
//! input/keyframe-request/clipboard in. Reuses the clipboard + cursor types from
//! [`crate::socket`]. The PoC framing was `[u32be len][payload]`; here the payload
//! is one of these tagged messages (JSON for control, raw Annex-B for video — see
//! [`VideoAu`]).

use serde::{Deserialize, Serialize};

pub use crate::config::ChromaMode;
pub use crate::socket::{
    ClipboardData, ClipboardOffer, ClipboardRequest, CursorMeta, CursorShape,
};

/// Server → viewer, sent **once at connect before any video frame** (port-1 tag 4):
/// the active chroma mode for this session. The viewer uses it to choose its decode
/// path — `Yuv420` decodes the `W×H` stream directly; `Yuv444` inserts the AVC444
/// reconstruction filter for the double-height `W×2H` stream. Global + fixed at the
/// server's launch, so it never changes within a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeMsg {
    pub chroma: ChromaMode,
}

/// One monitor's geometry in the viewer's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerMonitor {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Viewer → server, first frame: auth + capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ViewerHello {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// e.g. `["h264", "clipboard", "cursor"]`.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Server → viewer: the current monitor set (on connect + on selection change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorList {
    pub monitors: Vec<ViewerMonitor>,
}

/// Server → viewer: one H.264 access unit. The Annex-B bytes are carried out of
/// band (after this header) when binary-framed; in the JSON form they are base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoAu {
    pub monitor_id: u32,
    pub idr: bool,
    /// Presentation timestamp (ns).
    pub pts: u64,
    /// Annex-B access unit.
    #[serde(with = "crate::socket::serde_bytes_b64")]
    pub annexb: Vec<u8>,
}

/// Viewer → server: an input event tagged with its monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerInput {
    PointerMove { monitor_id: u32, x: f64, y: f64 },
    Button { button: i32, pressed: bool },
    Axis { axis: u32, step: i32 },
    Key { keysym: u32, pressed: bool },
}

/// Viewer → server: force an IDR (reconnect / seek).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestKeyframe {
    pub monitor_id: u32,
}

// --- authoritative view spec ------------------------------------------------------------
// The single message that describes the viewer's *entire* window set and each window's
// content, sent on every selection change / layout change / viewer connect (port-1 tag 3).
// The viewer keeps a stable set of N windows (N = configured monitor count) and only swaps
// their content per this spec — video AUs and `TermData` are pure content fills, they never
// create or destroy windows. Replaces the old daemon-reported layout + `TermInit` split.

/// One window in the viewer's stable set. `id` is the layout slot index (`0..n-1`); window 0
/// is always the "main" window (close button + Settings). Geometry is unified-desktop px,
/// taken from the server's **configured** layout (fleet-wide, always known — not the daemon's
/// live report), so the window set is identical across every clone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewMonitor {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// What the windows in a [`ViewSpec`] show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ViewContent {
    /// Headed clone: every window shows its monitor's H.264 desktop stream. (The video surface is
    /// clone-agnostic — switching between headed clones reuses the same decode windows — so no
    /// clone id is needed here.)
    Desktop,
    /// Headless clone: window 0 shows the tmux tabs (one per `sessions` entry); every other window
    /// shows a blank placeholder. `clone` is the selected host id: it identifies *which* clone the
    /// sessions belong to so the viewer rebuilds the terminal (fresh scrollback/grids) when the
    /// selection moves to a different headless clone — two clones can share a session name (`main`),
    /// so the name alone can't distinguish them.
    Terminal { clone: String, sessions: Vec<String> },
}

/// Server → viewer (port-1 tag 3): the complete view for the selected clone. `monitors` is
/// empty when nothing is selected — the viewer then shows only its keep-alive window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSpec {
    pub monitors: Vec<ViewMonitor>,
    pub content: ViewContent,
}

// --- headless-clone terminal (tmux) view ------------------------------------------------
// When the selected clone is headless (`Host.headless`), the control-server's `termplane`
// proxies each tmux session as a PTY over these port-1 messages instead of streaming video.
// The session list itself rides in [`ViewSpec`] (`ViewContent::Terminal`); the messages below
// carry the per-session byte streams and viewer→server input.

/// Server → viewer (port-1 tag 7): a chunk of raw terminal output for `session` (the bytes a
/// tmux client would write to its terminal — already VT/ANSI-encoded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermData {
    pub session: String,
    #[serde(with = "crate::socket::serde_bytes_b64")]
    pub data: Vec<u8>,
}

/// Viewer → server: keystrokes / pasted bytes for `session` (fed to the tmux client's stdin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermInput {
    pub session: String,
    #[serde(with = "crate::socket::serde_bytes_b64")]
    pub data: Vec<u8>,
}

/// Viewer → server: the terminal tab for `session` was resized (characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermResize {
    pub cols: u16,
    pub rows: u16,
}

/// Viewer → server: the tab-bar "+" — create a new tmux session in the selected headless clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TermNewSession {}

/// Server → viewer messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToViewer {
    MonitorList(MonitorList),
    Video(VideoAu),
    Cursor(CursorMeta),
    ClipboardOffer(ClipboardOffer),
    ClipboardData(ClipboardData),
}

/// Viewer → server messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromViewer {
    Hello(ViewerHello),
    Input(ViewerInput),
    RequestKeyframe(RequestKeyframe),
    ClipboardOffer(ClipboardOffer),
    ClipboardRequest(ClipboardRequest),
    ClipboardData(ClipboardData),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_viewer_tagged() {
        let m = ToViewer::Video(VideoAu { monitor_id: 0, idr: true, pts: 1, annexb: vec![0, 0, 1] });
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"t\":\"video\""));
        assert_eq!(serde_json::from_str::<ToViewer>(&s).unwrap(), m);
    }
}
