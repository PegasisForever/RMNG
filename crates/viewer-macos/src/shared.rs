//! State shared between the net thread (port-1 reader + decoders) and the AppKit main thread.
//!
//! The net thread only ever *latches* into these structures; the main thread reads them from
//! its wake-ups (frame arrived, spec changed) and its housekeeping tick. Same split as the GTK
//! viewer (`crates/viewer/src/main.rs`), minus GTK.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use viewer_core::auto_lock::AutoLock;
use viewer_core::forward::ForwardManager;
use wire::socket::{ClipboardMsg, CursorShape};
use wire::viewer::ViewSpec;

use crate::decoder::DecodedFrame;

/// Input/clipboard write half (None while disconnected).
pub type Writer = Arc<Mutex<Option<TcpStream>>>;
/// The server `host:port`, edited live by the Settings dialog, re-read on every reconnect.
pub type ServerAddr = Arc<Mutex<String>>;

/// How long to suppress local motion after a server-initiated cursor warp (an MCP move).
pub const WARP_SUPPRESS: Duration = Duration::from_millis(500);
/// How long the synthetic cursor overlay stays drawn after an agent-driven (warp) move.
pub const AGENT_CURSOR_SHOW: Duration = Duration::from_millis(1000);

/// Latest cursor state per monitor. The local OS cursor takes the remote shape; the synthetic
/// overlay is drawn only while `warp_until` is in the future (the agent is driving).
#[derive(Default, Clone)]
pub struct CursorEntry {
    pub x: i32,
    pub y: i32,
    pub shape: Option<CursorShape>,
    /// Bumps on every shape change so the main thread re-textures lazily.
    pub version: u64,
    pub warp_until: Option<Instant>,
}

/// The server's authoritative view spec; `epoch` bumps only on a real change so the main thread
/// reconciles its window set exactly when needed.
#[derive(Default)]
pub struct ViewState {
    pub spec: Option<ViewSpec>,
    pub epoch: u64,
}

/// The latest decoded frame per monitor (latest-wins, like a paintable sink).
pub type FrameSlots = Mutex<HashMap<u32, DecodedFrame>>;

pub struct Shared {
    pub writer: Writer,
    pub addr: ServerAddr,
    /// `0` = Yuv420, `1` = Yuv444 (AVC444 stacked stream) — the server's tag-4 handshake.
    pub chroma: AtomicU8,
    pub connected: AtomicBool,
    pub view: Mutex<ViewState>,
    pub cursors: Mutex<HashMap<u32, CursorEntry>>,
    /// Deadline until which local pointer motion is not sent (debounced per warp).
    pub warp: Mutex<Option<Instant>>,
    pub auto_lock: Mutex<AutoLock>,
    pub clip_inbox: Mutex<VecDeque<ClipboardMsg>>,
    pub term_out: Mutex<VecDeque<(String, Vec<u8>)>>,
    pub frames: FrameSlots,
    pub forwards: Arc<ForwardManager>,
    /// Wakes the main thread (a main-queue hop) after something above changed.
    pub wake: Box<dyn Fn(Wake) + Send + Sync>,
}

/// Why the main thread is being woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// A new decoded frame for this monitor is in `frames`.
    Frame(u32),
    /// The view spec, connection state, or address changed: reconcile windows / status.
    View,
    /// Cursor / clipboard / terminal data arrived.
    Data,
}

/// viewer → server framing: `[u8 tag][u32be len][json]`. tag 0 = input, 1 = clipboard,
/// 2 = forward status, 3 = terminal input, 4 = terminal resize, 5 = new terminal session.
pub fn send_tagged(writer: &Writer, tag: u8, json: &str) {
    // One guard for the whole op (a second lock on the error path would self-deadlock), and one
    // contiguous write so TCP_NODELAY emits a single segment per event.
    let mut guard = writer.lock().unwrap();
    if let Some(g) = guard.as_mut() {
        let body = json.as_bytes();
        let mut frame = Vec::with_capacity(1 + 4 + body.len());
        frame.push(tag);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        if g.write_all(&frame).is_err() {
            // Dead link surfaced on the write side: shut the shared socket so the net thread's
            // parked read returns now and the reconnect loop starts immediately.
            let _ = g.shutdown(std::net::Shutdown::Both);
            *guard = None;
        }
    }
}

/// An input event (tag 0) for the selected clone.
pub fn send_input(writer: &Writer, json: &str) {
    send_tagged(writer, 0, json);
}

#[allow(dead_code)]
pub fn is_text_mime(m: &str) -> bool {
    m.starts_with("text/plain") || m == "UTF8_STRING" || m == "TEXT"
}

/// Pick the MIMEs to fetch from a clipboard offer — up to one per category: best image,
/// `text/html`, best plain text (plain text alongside rich, so plain-text targets can paste).
#[allow(dead_code)]
pub fn pick_mimes(mimes: &[String]) -> Vec<String> {
    let image = mimes
        .iter()
        .find(|m| m.starts_with("image/png"))
        .or_else(|| mimes.iter().find(|m| m.starts_with("image/")));
    let html = mimes.iter().find(|m| *m == "text/html");
    let text = mimes
        .iter()
        .find(|m| m.starts_with("text/plain;charset=utf-8"))
        .or_else(|| mimes.iter().find(|m| is_text_mime(m)));
    let mut out: Vec<String> = [image, html, text].into_iter().flatten().cloned().collect();
    if out.is_empty() {
        out.extend(mimes.first().cloned()); // unknown-only offer: mirror it as-is
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_mimes_takes_one_per_category_and_keeps_plain_text() {
        let offer: Vec<String> =
            ["text/html", "text/plain;charset=utf-8", "image/png", "image/jpeg", "text/plain"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(pick_mimes(&offer), vec!["image/png", "text/html", "text/plain;charset=utf-8"]);
    }

    #[test]
    fn pick_mimes_mirrors_an_unknown_only_offer() {
        assert_eq!(pick_mimes(&["application/x-foo".to_string()]), vec!["application/x-foo"]);
    }
}
