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

/// The wake reasons that piled up since the main thread last drained them.
///
/// Wakes coalesce (one main-queue hop for a burst), so the reason has to survive the coalescing:
/// without it every wake meant a full reconcile-and-present of every window, and a moving remote
/// pointer alone (one `Data` per captured frame, per monitor) paid for a GPU encode and present
/// that had nothing to draw.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WakeSet {
    /// The spec / connection state changed. Recorded for completeness; the reconcile itself is
    /// still driven by the spec epoch, which cannot miss a change this flag could.
    pub view: bool,
    /// Cursor, clipboard or terminal bytes arrived.
    pub data: bool,
    /// Monitors with a new decoded frame — at most one entry each, and normally one entry total.
    pub frames: Vec<u32>,
}

impl WakeSet {
    pub fn add(&mut self, why: Wake) {
        match why {
            Wake::View => self.view = true,
            Wake::Data => self.data = true,
            // Linear scan over a handful of monitors beats a set, and keeps draw order stable.
            Wake::Frame(m) => {
                if !self.frames.contains(&m) {
                    self.frames.push(m);
                }
            }
        }
    }

    /// Fold `other`'s reasons in, keeping everything either side holds.
    pub fn merge(&mut self, other: &WakeSet) {
        self.view |= other.view;
        self.data |= other.data;
        for m in &other.frames {
            if !self.frames.contains(m) {
                self.frames.push(*m);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.view && !self.data && self.frames.is_empty()
    }
}

/// The reasons plus the "a main-thread drain is already on its way" flag, under one lock so the
/// two can never disagree: whoever flips the flag from false schedules the hop, and every reason
/// recorded while it is true is guaranteed to be seen by a drain that has not happened yet.
pub struct WakeQueue {
    inner: Mutex<WakeQueueInner>,
}

struct WakeQueueInner {
    pending: WakeSet,
    /// A drain has been scheduled and has not taken `pending` yet.
    scheduled: bool,
}

impl Default for WakeQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WakeQueue {
    pub const fn new() -> Self {
        WakeQueue {
            inner: Mutex::new(WakeQueueInner {
                pending: WakeSet { view: false, data: false, frames: Vec::new() },
                scheduled: false,
            }),
        }
    }

    /// Record a reason (net thread). Returns whether the caller must schedule the main-thread
    /// drain; `false` means an already-scheduled drain will pick this reason up.
    #[must_use]
    pub fn push(&self, why: Wake) -> bool {
        let mut g = self.inner.lock().unwrap();
        g.pending.add(why);
        !std::mem::replace(&mut g.scheduled, true)
    }

    /// Take everything recorded so far, re-arming scheduling *before* the refresh runs (main
    /// thread). That ordering is what makes the queue lossless: a reason arriving while the
    /// refresh is in flight — past the point where the refresh could still see it — finds
    /// `scheduled` false and schedules a fresh hop of its own.
    pub fn drain(&self) -> WakeSet {
        let mut g = self.inner.lock().unwrap();
        g.scheduled = false;
        std::mem::take(&mut g.pending)
    }

    /// Give reasons back that the drain could not act on. They are merged, so nothing that
    /// arrived in the meantime is overwritten, and the next wake carries them in.
    pub fn restore(&self, set: &WakeSet) {
        if set.is_empty() {
            return;
        }
        self.inner.lock().unwrap().pending.merge(set);
    }
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

    #[test]
    fn wake_set_keeps_every_reason_and_lists_each_monitor_once() {
        let mut s = WakeSet::default();
        assert!(s.is_empty());
        for w in [Wake::Frame(1), Wake::Data, Wake::Frame(0), Wake::Frame(1), Wake::View] {
            s.add(w);
        }
        assert!(s.view && s.data);
        assert_eq!(s.frames, vec![1, 0]);
    }

    #[test]
    fn wake_queue_schedules_one_drain_per_burst() {
        let q = WakeQueue::new();
        assert!(q.push(Wake::Frame(0)), "the first wake of a burst schedules the hop");
        assert!(!q.push(Wake::Frame(1)), "the rest of the burst folds into it");
        assert!(!q.push(Wake::Data));
        let taken = q.drain();
        assert_eq!(taken.frames, vec![0, 1]);
        assert!(taken.data);
        assert!(q.drain().is_empty());
        assert!(q.push(Wake::View), "after a drain the next wake schedules again");
    }

    #[test]
    fn wake_queue_keeps_a_reason_that_arrives_mid_refresh() {
        let q = WakeQueue::new();
        assert!(q.push(Wake::Frame(0)));
        // The drain marks the start of the refresh; everything after it belongs to the next one.
        assert_eq!(q.drain().frames, vec![0]);
        assert!(q.push(Wake::Frame(1)), "a wake during the refresh schedules a fresh hop");
        assert_eq!(q.drain().frames, vec![1]);
    }

    #[test]
    fn wake_queue_restores_unhandled_reasons_without_dropping_new_ones() {
        let q = WakeQueue::new();
        assert!(q.push(Wake::Frame(0)));
        let taken = q.drain();
        assert!(q.push(Wake::Data), "a new reason arrived before the restore");
        q.restore(&taken);
        let next = q.drain();
        assert_eq!(next.frames, vec![0]);
        assert!(next.data);
    }

    #[test]
    fn wake_queue_loses_nothing_when_wakes_race_the_drain() {
        use std::collections::HashSet;
        use std::sync::mpsc;

        let q = Arc::new(WakeQueue::new());
        let (tx, rx) = mpsc::channel();
        let pushers: Vec<_> = (0..4u32)
            .map(|mon| {
                let (q, tx) = (q.clone(), tx.clone());
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        // Exactly what the net thread does: record, then schedule if asked to.
                        if q.push(Wake::Frame(mon)) {
                            tx.send(()).unwrap();
                        }
                    }
                })
            })
            .collect();
        drop(tx);
        // The "main thread": one drain per scheduled hop.
        let mut seen: HashSet<u32> = HashSet::new();
        for () in rx {
            seen.extend(q.drain().frames);
        }
        for p in pushers {
            p.join().unwrap();
        }
        assert_eq!(seen, (0..4).collect::<HashSet<u32>>());
        // The last wake before the queue went quiet either scheduled a hop of its own or landed
        // in one still to come; either way nothing may be sitting here unclaimed.
        assert!(q.drain().is_empty(), "a wake was recorded with no drain left to see it");
    }
}
