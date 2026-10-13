//! The browser viewer ⇄ control-server protocol: one WebSocket per viewer window, on the web
//! port at `/api/viewer?monitor=<id>`. The socket carries video for that one monitor and every
//! control message.
//!
//! - A **binary** message starts with a kind byte. Kind [`KIND_VIDEO`] (server → browser) is
//!   a [`VideoHeader`] ([`VIDEO_HEADER_LEN`] bytes), then one Annex-B access unit. Kind
//!   [`KIND_CLIPBOARD_DATA`] (both directions) is one clipboard reply
//!   ([`encode_clipboard_data`]), so an image crosses as raw bytes, not base64 in JSON.
//! - Everything else is a **JSON text** message: [`ToBrowser`] out, [`FromBrowser`] in. Both
//!   reuse the port-1 types, so the server's handlers are the same for both kinds of viewer.
//!   A `ClipboardMsg::Data` never travels as JSON here.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::socket::{ClipboardData, ClipboardMsg, CursorMeta, InputMsg};
use crate::viewer::{ModeMsg, RequestKeyframe, TermData, TermInput, TermResize, ViewSpec};

/// Byte 0 of a binary message: a video frame ([`VideoHeader`]).
pub const KIND_VIDEO: u8 = 0;
/// Byte 0 of a binary message: one clipboard reply ([`encode_clipboard_data`]).
pub const KIND_CLIPBOARD_DATA: u8 = 1;

/// Size of [`VideoHeader`] on the wire.
pub const VIDEO_HEADER_LEN: usize = 18;

/// The header in front of every video message. Big-endian:
///
/// | bytes | field |
/// |---|---|
/// | 0 | kind, always 0 (video) |
/// | 1 | flags, bit 0 = keyframe |
/// | 2..6 | monitor id (u32) |
/// | 6..10 | frame sequence number per monitor encoder (u32, wraps) |
/// | 10..18 | server time the encoder produced the frame, microseconds since the Unix epoch (u64) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub monitor_id: u32,
    pub seq: u32,
    pub key: bool,
    pub server_us: u64,
}

impl VideoHeader {
    pub fn encode(&self) -> [u8; VIDEO_HEADER_LEN] {
        let mut b = [0u8; VIDEO_HEADER_LEN];
        b[1] = self.key as u8;
        b[2..6].copy_from_slice(&self.monitor_id.to_be_bytes());
        b[6..10].copy_from_slice(&self.seq.to_be_bytes());
        b[10..18].copy_from_slice(&self.server_us.to_be_bytes());
        b
    }
}

/// One clipboard reply as a binary message. Big-endian:
///
/// | bytes | field |
/// |---|---|
/// | 0 | kind, always [`KIND_CLIPBOARD_DATA`] |
/// | 1..9 | offer serial (u64) |
/// | 9..11 | MIME type length `n` (u16) |
/// | 11..11+n | MIME type, UTF-8 |
/// | 11+n.. | the bytes (empty = the owner could not read them) |
pub fn encode_clipboard_data(d: &ClipboardData) -> Vec<u8> {
    let mime = d.mime_type.as_bytes();
    let mut out = Vec::with_capacity(11 + mime.len() + d.bytes.len());
    out.push(KIND_CLIPBOARD_DATA);
    out.extend_from_slice(&d.serial.to_be_bytes());
    out.extend_from_slice(&(mime.len() as u16).to_be_bytes());
    out.extend_from_slice(mime);
    out.extend_from_slice(&d.bytes);
    out
}

/// Read a binary message written by [`encode_clipboard_data`]; `None` for any other message.
pub fn decode_clipboard_data(b: &[u8]) -> Option<ClipboardData> {
    if b.len() < 11 || b[0] != KIND_CLIPBOARD_DATA {
        return None;
    }
    let serial = u64::from_be_bytes(b[1..9].try_into().ok()?);
    let n = u16::from_be_bytes(b[9..11].try_into().ok()?) as usize;
    let mime = b.get(11..11 + n)?;
    Some(ClipboardData {
        serial,
        mime_type: String::from_utf8(mime.to_vec()).ok()?,
        bytes: b[11 + n..].to_vec(),
    })
}

/// Server → browser, as a JSON text message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "t", rename_all = "snake_case")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum ToBrowser {
    Mode(ModeMsg),
    ViewSpec(ViewSpec),
    Cursor(CursorMeta),
    Clipboard(ClipboardMsg),
    TermData(TermData),
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
    /// Encode the monitor's next frame as a keyframe: at connect, and after a decode error.
    RequestKeyframe(RequestKeyframe),
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
            server_us: 0x1112_1314_1516_1718,
        };
        assert_eq!(
            h.encode(),
            [
                0, 1, 1, 2, 3, 4, 5, 6, 7, 8, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18
            ]
        );
        assert_eq!(VideoHeader { key: false, ..h }.encode()[1], 0);
    }

    /// The frontend's `parseClipboardData` test reads these same bytes; keep the two in step.
    #[test]
    fn clipboard_data_layout() {
        let d = ClipboardData {
            // Under 2^53, as every real serial is: the browser reads it as a JS number.
            serial: 0x0001_0203_0405_0607,
            mime_type: "text/html".into(),
            bytes: b"<b>".to_vec(),
        };
        let b = encode_clipboard_data(&d);
        assert_eq!(
            b,
            [
                &[1, 0, 1, 2, 3, 4, 5, 6, 7, 0, 9][..],
                b"text/html",
                b"<b>"
            ]
            .concat()
        );
        assert_eq!(decode_clipboard_data(&b), Some(d));
        // Empty bytes are a reply too (the owner could not read them).
        let empty = ClipboardData {
            serial: 1,
            mime_type: "image/png".into(),
            bytes: vec![],
        };
        assert_eq!(
            decode_clipboard_data(&encode_clipboard_data(&empty)),
            Some(empty)
        );
        // A video message, or one cut short inside the MIME type, is not clipboard data.
        assert_eq!(decode_clipboard_data(&[0; 20]), None);
        assert_eq!(decode_clipboard_data(&b[..15]), None);
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
    }
}
