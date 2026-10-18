//! The browser viewer ⇄ control-server protocol: one WebSocket per viewer window, on the web
//! port at `/api/viewer?monitor=<id>`. The socket carries video for that one monitor and every
//! control message.
//!
//! - A **binary** message starts with a kind byte. Kind [`KIND_VIDEO`] (server → browser) is
//!   a [`VideoHeader`] ([`VIDEO_HEADER_LEN`] bytes), then one Annex-B access unit. Kinds
//!   [`KIND_CLIPBOARD_PIECE`] and [`KIND_CLIPBOARD_DATA`] (both directions) carry one clipboard
//!   reply in pieces ([`encode_clipboard_piece`]), so an image crosses as raw bytes, not base64
//!   in JSON, and never holds the socket for long.
//! - Everything else is a **JSON text** message: [`ToBrowser`] out, [`FromBrowser`] in. Input,
//!   cursor and clipboard reuse the clone-daemon socket types ([`crate::socket`]). A
//!   `ClipboardMsg::Data` never travels as JSON here.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::socket::{ClipboardData, ClipboardMsg, CursorMeta, InputMsg};

/// Byte 0 of a binary message: a video frame ([`VideoHeader`]).
pub const KIND_VIDEO: u8 = 0;
/// Byte 0 of a binary message: the last (or only) piece of a clipboard reply
/// ([`encode_clipboard_piece`]).
pub const KIND_CLIPBOARD_DATA: u8 = 1;
/// Byte 0 of a binary message: a piece of a clipboard reply, and more pieces follow.
pub const KIND_CLIPBOARD_PIECE: u8 = 2;
/// A clipboard reply crosses in pieces of at most this many bytes, so that video can go
/// between them.
pub const CLIPBOARD_PIECE_BYTES: usize = 64 * 1024;

/// Size of [`VideoHeader`] on the wire.
pub const VIDEO_HEADER_LEN: usize = 10;

/// The header in front of every video message. Big-endian:
///
/// | bytes | field |
/// |---|---|
/// | 0 | kind, always 0 (video) |
/// | 1 | flags, bit 0 = keyframe |
/// | 2..6 | monitor id (u32) |
/// | 6..10 | frame sequence number per monitor encoder (u32, wraps) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub monitor_id: u32,
    pub seq: u32,
    pub key: bool,
}

impl VideoHeader {
    pub fn encode(&self) -> [u8; VIDEO_HEADER_LEN] {
        let mut b = [0u8; VIDEO_HEADER_LEN];
        b[1] = self.key as u8;
        b[2..6].copy_from_slice(&self.monitor_id.to_be_bytes());
        b[6..10].copy_from_slice(&self.seq.to_be_bytes());
        b
    }
}

/// One piece of the clipboard reply `d` (`bytes` is that piece of `d.bytes`) as a binary
/// message. The reply is the pieces of one serial and MIME type in order; `last` marks its end.
/// Big-endian:
///
/// | bytes | field |
/// |---|---|
/// | 0 | kind: [`KIND_CLIPBOARD_PIECE`], or [`KIND_CLIPBOARD_DATA`] for the last piece |
/// | 1..9 | offer serial (u64) |
/// | 9..11 | MIME type length `n` (u16) |
/// | 11..11+n | MIME type, UTF-8 |
/// | 11+n.. | the piece (a reply with no bytes = the owner could not read them) |
pub fn encode_clipboard_piece(d: &ClipboardData, bytes: &[u8], last: bool) -> Vec<u8> {
    let mime = d.mime_type.as_bytes();
    let mut out = Vec::with_capacity(11 + mime.len() + bytes.len());
    out.push(if last { KIND_CLIPBOARD_DATA } else { KIND_CLIPBOARD_PIECE });
    out.extend_from_slice(&d.serial.to_be_bytes());
    out.extend_from_slice(&(mime.len() as u16).to_be_bytes());
    out.extend_from_slice(mime);
    out.extend_from_slice(bytes);
    out
}

/// Read a binary message written by [`encode_clipboard_piece`]: the piece (as a reply with only
/// its bytes) and whether it is the last. `None` for any other message.
pub fn decode_clipboard_piece(b: &[u8]) -> Option<(ClipboardData, bool)> {
    if b.len() < 11 || !matches!(b[0], KIND_CLIPBOARD_DATA | KIND_CLIPBOARD_PIECE) {
        return None;
    }
    let serial = u64::from_be_bytes(b[1..9].try_into().ok()?);
    let n = u16::from_be_bytes(b[9..11].try_into().ok()?) as usize;
    let mime = b.get(11..11 + n)?;
    let piece = ClipboardData {
        serial,
        mime_type: String::from_utf8(mime.to_vec()).ok()?,
        bytes: b[11 + n..].to_vec(),
    };
    Some((piece, b[0] == KIND_CLIPBOARD_DATA))
}

/// Server → browser, as a JSON text message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "t", rename_all = "snake_case")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum ToBrowser {
    /// The server's build, first on every connection. A window that reconnects after an
    /// update sees a different one and reloads, as the dashboard does on its `version` event.
    Version { build_id: String },
    /// How to read wheel events: the active layout preset's scroll mode and speed (percent).
    /// Sent at connect and when either changes.
    Scroll { mode: crate::control::ScrollMode, speed: f64 },
    ViewSpec(ViewSpec),
    Cursor(CursorMeta),
    Clipboard(ClipboardMsg),
    TermData(TermData),
    /// The answer to [`FromBrowser::Echo`], with the same `id`.
    Echo { id: u32 },
    /// How many clipboard pieces from the browser arrived on this socket (wraps). The browser
    /// keeps the pieces on the way within the lowest piece round trip and 100 ms of the rate
    /// these show (and always one), so its acknowledgements and input wait about 100 ms behind
    /// an upload.
    ClipAck { n: u32 },
}

/// Browser → server, as a JSON text message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "t", rename_all = "snake_case")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum FromBrowser {
    Input(InputMsg),
    Clipboard(ClipboardMsg),
    TermInput(TermInput),
    TermResize(TermResize),
    TermNewSession,
    /// Encode the monitor's next frame as a keyframe: after a decode error. A join gets one
    /// without asking.
    RequestKeyframe(RequestKeyframe),
    /// The newest video frame decoded (`seq` from its [`VideoHeader`]; a frame the window drops
    /// counts when it arrives) and how long the window held that number before this message
    /// (`held_ms`), how many clipboard pieces arrived on this socket (`clip`, wraps), and how
    /// many frames wait in the window's decoder (`decoding`). Sent every 50 ms when `seq`
    /// changed, at once when `clip` did, and once a second otherwise. The server takes the
    /// round trip, how far behind the window is and the delivered rate from it, keeps the
    /// clipboard pieces on the way within one round trip and 100 ms of that rate, and does not
    /// lower the bitrate for a window whose decoder is behind.
    Ack {
        seq: u32,
        held_ms: u32,
        #[serde(default)]
        clip: u32,
        #[serde(default)]
        decoding: u32,
    },
    /// Answered with [`ToBrowser::Echo`], ahead of the video waiting in this socket's queue
    /// (as every control message), so the time to the answer is the round trip plus the wait
    /// in the kernel send buffer and any proxy on the way. `rmng-fps` measures it; the viewer
    /// does not send it.
    Echo { id: u32 },
}

/// Browser → server: encode the monitor's next frame as a keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct RequestKeyframe {
    pub monitor_id: u32,
}

/// One monitor of the view; the browser opens one window per monitor. `id` is the layout slot
/// index (`0..n-1`). Geometry is unified-desktop px, taken from the server's **configured**
/// layout (fleet-wide, always known — not the daemon's live report), so the window set is the
/// same for every clone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ViewMonitor {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// What the windows in a [`ViewSpec`] show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "mode", rename_all = "snake_case")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum ViewContent {
    /// Headed clone: every window shows its monitor's H.264 desktop stream. (The video surface is
    /// clone-agnostic, so no clone id is needed here.)
    Desktop,
    /// Headless clone: a terminal with one tab per tmux session in `sessions`, and no video.
    /// `clone` is the selected clone id, so the viewer rebuilds the terminal (fresh scrollback)
    /// when the selection moves to another headless clone: two clones can share a session name
    /// (`main`), so the name alone does not tell them apart.
    Terminal {
        clone: String,
        sessions: Vec<String>,
    },
}

/// Server → browser: the complete view for the selected clone, sent at connect and on every
/// selection or layout change. `monitors` is empty when nothing is selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ViewSpec {
    pub monitors: Vec<ViewMonitor>,
    pub content: ViewContent,
}

/// Server → browser: a chunk of raw terminal output for `session` (the bytes a
/// tmux client would write to its terminal — already VT/ANSI-encoded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct TermData {
    pub session: String,
    #[serde(with = "crate::socket::serde_bytes_b64")]
    #[ts(type = "string")]
    pub data: Vec<u8>,
}

/// Browser → server: keystrokes / pasted bytes for `session` (fed to the tmux client's stdin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct TermInput {
    pub session: String,
    #[serde(with = "crate::socket::serde_bytes_b64")]
    #[ts(type = "string")]
    pub data: Vec<u8>,
}

/// Browser → server: the terminal for `session` was resized (characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct TermResize {
    pub cols: u16,
    pub rows: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::ClipboardOffer;

    /// The frontend's `parseVideoHeader` test reads these same bytes; keep the two in step.
    #[test]
    fn video_header_layout() {
        let h = VideoHeader {
            monitor_id: 0x0102_0304,
            seq: 0x0506_0708,
            key: true,
        };
        assert_eq!(h.encode(), [0, 1, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(VideoHeader { key: false, ..h }.encode()[1], 0);
    }

    /// The frontend's `parseClipboardPiece` test reads these same bytes; keep the two in step.
    #[test]
    fn clipboard_piece_layout() {
        let d = ClipboardData {
            // Under 2^53, as every real serial is: the browser reads it as a JS number.
            serial: 0x0001_0203_0405_0607,
            mime_type: "text/html".into(),
            bytes: b"<b>x</b>".to_vec(),
        };
        let b = encode_clipboard_piece(&d, b"<b>", false);
        assert_eq!(
            b,
            [
                &[2, 0, 1, 2, 3, 4, 5, 6, 7, 0, 9][..],
                b"text/html",
                b"<b>"
            ]
            .concat()
        );
        let piece = |bytes: &[u8]| ClipboardData {
            bytes: bytes.to_vec(),
            ..d.clone()
        };
        assert_eq!(decode_clipboard_piece(&b), Some((piece(b"<b>"), false)));
        let b = encode_clipboard_piece(&d, b"x</b>", true);
        assert_eq!(b[0], KIND_CLIPBOARD_DATA);
        assert_eq!(decode_clipboard_piece(&b), Some((piece(b"x</b>"), true)));
        // Empty bytes are a reply too (the owner could not read them).
        assert_eq!(
            decode_clipboard_piece(&encode_clipboard_piece(&d, &[], true)),
            Some((piece(&[]), true))
        );
        // A video message, or one cut short inside the MIME type, is not clipboard data.
        assert_eq!(decode_clipboard_piece(&[0; 20]), None);
        assert_eq!(decode_clipboard_piece(&b[..15]), None);
    }

    /// The messages nest one internally tagged enum in another (`t` outside, `k` or `kind`
    /// inside). Pin that both tags survive a round trip.
    #[test]
    fn nested_tags_round_trip() {
        let out = ToBrowser::Clipboard(ClipboardMsg::Offer(ClipboardOffer {
            serial: 3,
            mime_types: vec!["text/plain".into()],
        }));
        let s = serde_json::to_string(&out).unwrap();
        assert_eq!(
            s,
            r#"{"t":"clipboard","k":"offer","serial":3,"mime_types":["text/plain"]}"#
        );
        assert_eq!(serde_json::from_str::<ToBrowser>(&s).unwrap(), out);

        let input: FromBrowser =
            serde_json::from_str(r#"{"t":"input","kind":"button","button":272,"pressed":true}"#)
                .unwrap();
        assert_eq!(
            input,
            FromBrowser::Input(InputMsg::Button {
                button: 272,
                pressed: true
            })
        );
        let key: FromBrowser =
            serde_json::from_str(r#"{"t":"request_keyframe","monitor_id":1}"#).unwrap();
        assert_eq!(
            key,
            FromBrowser::RequestKeyframe(RequestKeyframe { monitor_id: 1 })
        );
        let new: FromBrowser = serde_json::from_str(r#"{"t":"term_new_session"}"#).unwrap();
        assert_eq!(new, FromBrowser::TermNewSession);
        let ack: FromBrowser =
            serde_json::from_str(r#"{"t":"ack","seq":9,"held_ms":12,"clip":3,"decoding":2}"#)
                .unwrap();
        assert_eq!(
            ack,
            FromBrowser::Ack {
                seq: 9,
                held_ms: 12,
                clip: 3,
                decoding: 2
            }
        );
        // A window of an older build sends neither `clip` nor `decoding`.
        let ack: FromBrowser =
            serde_json::from_str(r#"{"t":"ack","seq":9,"held_ms":12}"#).unwrap();
        assert!(matches!(ack, FromBrowser::Ack { clip: 0, decoding: 0, .. }));
        let echo: FromBrowser = serde_json::from_str(r#"{"t":"echo","id":7}"#).unwrap();
        assert_eq!(echo, FromBrowser::Echo { id: 7 });
        assert_eq!(
            serde_json::to_string(&ToBrowser::Echo { id: 7 }).unwrap(),
            r#"{"t":"echo","id":7}"#
        );
        assert_eq!(
            serde_json::to_string(&ToBrowser::ClipAck { n: 3 }).unwrap(),
            r#"{"t":"clip_ack","n":3}"#
        );
    }
}
