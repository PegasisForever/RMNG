//! The clone-daemon ⇄ control-server unix-socket protocol (`SOCK_SEQPACKET`).
//!
//! dmabuf fds ride alongside `FrameMsg` via `SCM_RIGHTS` (out of band — not in the
//! serialized struct). All other messages are length-delimited JSON for now (a
//! binary framing is an option later). Cursor is **not** composited into frames;
//! it travels as [`CursorMeta`]. Clipboard is rich + lazy via the offer/request/
//! data triple, brokered centrally by control-server.

use serde::{Deserialize, Serialize};

/// A captured monitor frame descriptor. The dmabuf fd(s) are passed via SCM_RIGHTS
/// in the same datagram, in plane order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameMsg {
    pub monitor_id: u32,
    /// DRM fourcc (e.g. `AR24`).
    pub fourcc: u32,
    /// DRM format modifier.
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub planes: Vec<PlaneLayout>,
    /// Monotonic per-monitor sequence; echoed back in [`Ack`].
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneLayout {
    pub offset: u32,
    pub stride: u32,
}

/// Cursor metadata (cursor-mode METADATA — never composited into the frame).
/// `shape` is sent only when it changes; position updates carry `shape: None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorMeta {
    pub monitor_id: u32,
    pub x: i32,
    pub y: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<CursorShape>,
    /// True when this update is a server-initiated **warp** (an MCP-injected pointer
    /// move) rather than the passive echo of the user's own motion. The viewer snaps
    /// the drawn cursor to it and briefly suppresses local pointer-motion sends.
    #[serde(default, skip_serializing_if = "is_false")]
    pub warp: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    /// Raw RGBA8888, `width * height * 4` bytes.
    #[serde(with = "serde_bytes_b64")]
    pub rgba: Vec<u8>,
}

/// One monitor's **actual** placement in the unified desktop (pixels), reported by the
/// daemon (after it applies the configured layout) → server → viewer. The viewer routes
/// cross-window drags against this real layout instead of assuming left-to-right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorPlacement {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// An input event to inject into a clone via Mutter RemoteDesktop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputMsg {
    /// Absolute pointer position within a monitor.
    PointerMove { monitor_id: u32, x: f64, y: f64 },
    /// Relative (unaccelerated) pointer motion — for pointer-lock / games. No monitor
    /// id: Mutter applies the delta to the focused surface's current position.
    PointerRelative { dx: f64, dy: f64 },
    /// Mouse button (evdev code, e.g. 0x110 = BTN_LEFT) press/release.
    Button { button: i32, pressed: bool },
    /// Discrete scroll: axis 0 = vertical, 1 = horizontal; step ±1.
    Axis { axis: u32, step: i32 },
    /// Key by X11 keysym (used by the MCP `key`/`type` tools for text/combos).
    Key { keysym: u32, pressed: bool },
    /// Key by evdev keycode (the viewer supplies `hardware_keycode - 8`) — faithful
    /// physical-key identity so games that read raw keys (Minecraft/GLFW) behave.
    KeyCode { keycode: u32, pressed: bool },
}

/// Releases the held PipeWire buffer for `(monitor_id, seq)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub monitor_id: u32,
    pub seq: u64,
}

/// Server → daemon: start/stop the continuous per-monitor feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    pub stream: bool,
}

/// Server → daemon: deliver one frame on demand (screenshot path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRequest {
    pub monitor_id: u32,
}

/// Clipboard (rich + lazy). The broker fans `Offer`s out and serves bytes on
/// `Request`. Used on both the socket and the viewer protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardOffer {
    pub serial: u64,
    pub mime_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardRequest {
    pub serial: u64,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardData {
    pub serial: u64,
    pub mime_type: String,
    #[serde(with = "serde_bytes_b64")]
    pub bytes: Vec<u8>,
}

/// daemon → server, first message on connect: identifies the clone (so the server
/// can route a shared bind-mounted socket by clone id rather than peer address).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub clone_id: String,
}

/// Top-level framed message daemon → server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DaemonMsg {
    Hello(Hello),
    Frame(FrameMsg),
    Cursor(CursorMeta),
    /// The clone's actual monitor layout (after the daemon applies the configured one).
    /// A struct variant (not a bare `Vec`) so it serializes under the internal `t` tag.
    Layout { monitors: Vec<MonitorPlacement> },
    /// A clone app put something on the clipboard — advertises the MIME types
    /// (rich + lazy: bytes are fetched only on [`ClipboardRequest`]).
    ClipboardOffer(ClipboardOffer),
    /// A clone app is pasting a *remote* selection — ask the broker for the bytes.
    ClipboardRequest(ClipboardRequest),
    /// Bytes for an earlier request (the daemon `SelectionRead` its clone clipboard).
    ClipboardData(ClipboardData),
}

/// Clipboard message on the **viewer** protocol (port-1 tag 1), both directions.
/// Rich + lazy: `Offer` advertises types, `Request` fetches one, `Data` delivers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum ClipboardMsg {
    Offer(ClipboardOffer),
    Request(ClipboardRequest),
    Data(ClipboardData),
}

/// Top-level framed message server → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Subscribe(Subscribe),
    FrameRequest(FrameRequest),
    Ack(Ack),
    Input(InputMsg),
    ClipboardOffer(ClipboardOffer),
    ClipboardRequest(ClipboardRequest),
    ClipboardData(ClipboardData),
}

/// Base64 (de)serialization for binary blobs in JSON framing. Swap for raw bytes
/// if/when the framing goes binary.
pub(crate) mod serde_bytes_b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
            out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
            out.push(if chunk.len() > 1 { ALPHABET[(n >> 6 & 63) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
        }
        s.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        let mut table = [255u8; 256];
        for (i, &c) in ALPHABET.iter().enumerate() {
            table[c as usize] = i as u8;
        }
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=' && !b.is_ascii_whitespace()).collect();
        for chunk in bytes.chunks(4) {
            let mut n = 0u32;
            let mut bits = 0;
            for &c in chunk {
                let v = table[c as usize];
                if v == 255 {
                    return Err(serde::de::Error::custom("invalid base64"));
                }
                n = (n << 6) | v as u32;
                bits += 6;
            }
            n <<= 24 - bits;
            let nbytes = bits / 8;
            for i in 0..nbytes {
                out.push((n >> (16 - i * 8)) as u8);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_msg_tagged_roundtrip() {
        let m = InputMsg::PointerMove { monitor_id: 0, x: 12.0, y: 34.0 };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"kind\":\"pointer_move\""));
        assert_eq!(serde_json::from_str::<InputMsg>(&s).unwrap(), m);
    }

    #[test]
    fn clipboard_msg_tags() {
        let offer = ClipboardMsg::Offer(ClipboardOffer { serial: 1, mime_types: vec!["text/html".into()] });
        let s = serde_json::to_string(&offer).unwrap();
        assert!(s.contains("\"k\":\"offer\""), "{s}");
        assert_eq!(serde_json::from_str::<ClipboardMsg>(&s).unwrap(), offer);

        let req = DaemonMsg::ClipboardRequest(ClipboardRequest { serial: 2, mime_type: "image/png".into() });
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"t\":\"clipboard_request\""), "{s}");
        assert_eq!(serde_json::from_str::<DaemonMsg>(&s).unwrap(), req);
    }

    #[test]
    fn base64_roundtrips() {
        for case in [vec![], vec![0u8], vec![1, 2, 3], (0u8..=255).collect::<Vec<_>>()] {
            let data = ClipboardData { serial: 1, mime_type: "x".into(), bytes: case.clone() };
            let s = serde_json::to_string(&data).unwrap();
            let back: ClipboardData = serde_json::from_str(&s).unwrap();
            assert_eq!(back.bytes, case);
        }
    }
}
