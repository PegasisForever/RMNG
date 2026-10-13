//! The browser viewer ⇄ control-server protocol: one WebSocket per viewer window, on the web
//! port at `/api/viewer?monitor=<id>`. The socket carries video for that one monitor and every
//! control message.
//!
//! - Video is a **binary** message: a [`VideoHeader`] ([`VIDEO_HEADER_LEN`] bytes), then one
//!   Annex-B access unit.
//! - Everything else is a **JSON text** message: [`ToBrowser`] out, [`FromBrowser`] in. Both
//!   reuse the port-1 types, so the server's handlers are the same for both kinds of viewer.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::socket::{ClipboardMsg, CursorMeta, InputMsg};
use crate::viewer::{ModeMsg, RequestKeyframe, TermData, TermInput, TermResize, ViewSpec};

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
