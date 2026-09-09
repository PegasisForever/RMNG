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
    /// The fd is a **memfd of system memory**, not a dmabuf: the clone has no GPU, so
    /// Mutter's screencast hands out shm buffers. The pixels are the same AR24/BGRA and
    /// `planes` still gives the real (offset, stride), but the server must map the fd
    /// instead of importing it. `serde(default)` keeps an older daemon (which never sends
    /// the field) reading as dmabuf, so a mixed fleet during a rollout still works.
    #[serde(default)]
    pub shm: bool,
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
    /// True when the clone hid its cursor (empty or fully-transparent sprite). A
    /// sustained hidden cursor is a pointer grab (a game): the viewer's auto
    /// pointer-lock keys on this. Only hide transitions carry it; a `shape`-bearing
    /// update marks the cursor visible again, and position-only updates say nothing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
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
    /// Smooth/finger scroll (touchpad). Deltas are surface pixels; `flags` match Mutter
    /// RemoteDesktop `NotifyPointerAxis` (1=finish, 2=wheel, 4=finger, 8=continuous).
    AxisContinuous { dx: f64, dy: f64, flags: u32 },
    /// Key by X11 keysym (used by the MCP `key`/`type` tools for text/combos).
    Key { keysym: u32, pressed: bool },
    /// Key by evdev keycode (the viewer supplies `hardware_keycode - 8`) — faithful
    /// physical-key identity so games that read raw keys (Minecraft/GLFW) behave.
    KeyCode { keycode: u32, pressed: bool },
}

/// Mutter `NotifyPointerAxis` flags (org.gnome.Mutter.RemoteDesktop.Session).
pub mod axis_flags {
    pub const FINISH: u32 = 1 << 0;
    pub const SOURCE_WHEEL: u32 = 1 << 1;
    pub const SOURCE_FINGER: u32 = 1 << 2;
    pub const SOURCE_CONTINUOUS: u32 = 1 << 3;
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
    /// True when this daemon had to start the session holder, so the desktop behind it was
    /// built seconds ago and holds no window anyone placed.
    ///
    /// A layout otherwise reaches a clone only while the operator is watching it, which keeps
    /// a preset change from rebuilding every clone's session at once. That rule protects
    /// window positions, and a session this new has none: the holder came up on whatever it
    /// remembered, or on the built-in single monitor when it remembered nothing. So the server
    /// pushes the active preset here instead of leaving the clone on a layout nobody chose.
    ///
    /// Defaults to false so a daemon older than this field reads as "the holder was already
    /// running", which is the case that must not trigger a push.
    #[serde(default)]
    pub fresh_session: bool,
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
    Layout {
        monitors: Vec<MonitorPlacement>,
    },
    /// A clone app put something on the clipboard — advertises the MIME types
    /// (rich + lazy: bytes are fetched only on [`ClipboardRequest`]).
    ClipboardOffer(ClipboardOffer),
    /// A clone app is pasting a *remote* selection — ask the broker for the bytes.
    ClipboardRequest(ClipboardRequest),
    /// Bytes for an earlier request (the daemon `SelectionRead` its clone clipboard).
    ClipboardData(ClipboardData),
    /// An unrecognized message (a `t` tag this build doesn't know). Kept for **forward
    /// compatibility**: a peer that predates a newer variant deserializes it to `Unknown`
    /// and ignores it, instead of treating the decode as a fatal error and dropping the
    /// connection. Never constructed/sent by us — only produced by deserialization.
    #[serde(other)]
    Unknown,
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
    /// Apply a new monitor layout live (no session restart, apps stay open). The daemon
    /// rebuilds a fresh Mutter session with this set, switches capture + input to it, then
    /// stops the old session (make-before-break). Sent on the daemon's `Hello` and on every
    /// `POST /api/layout/activate`.
    SetMonitors {
        monitors: Vec<crate::control::MonitorSpec>,
    },
    /// Start or stop capturing this clone's monitors.
    ///
    /// Capture is what makes the compositor paint: a clone with a screencast consumer
    /// repaints on every damage event whether or not anyone is watching the result, and the
    /// server drops those frames for every clone but the selected one. Measured on a
    /// software-rendered clone, that waste is most of its cost (8.4 CPU-seconds per 10
    /// seconds of a busy desktop, against 1.8 with no consumer attached).
    ///
    /// So the server keeps capture on only while a viewer is watching this clone, and the
    /// daemon wakes it briefly on its own for an on-demand screenshot. An older daemon
    /// decodes this to `Unknown` and keeps capturing, which is the previous behaviour.
    Capture {
        active: bool,
    },
    ClipboardOffer(ClipboardOffer),
    ClipboardRequest(ClipboardRequest),
    ClipboardData(ClipboardData),
    /// An unrecognized message (a `t` tag this build doesn't know). Kept for **forward
    /// compatibility**: an old daemon deserializes a future server→daemon variant to
    /// `Unknown` and ignores it, instead of a fatal decode error that would crash-loop it
    /// (its reader `exit(1)`s on a recv error). Never constructed/sent by us.
    #[serde(other)]
    Unknown,
}

/// Splitting one oversized message across several `SOCK_SEQPACKET` datagrams.
///
/// **Why this exists.** The media socket is `SOCK_SEQPACKET`, and the kernel refuses any
/// datagram larger than the sending socket's `SO_SNDBUF - 32`. That buffer defaults to
/// `net.core.wmem_default`, 212,992 bytes on this fleet, so the real ceiling is about 208 KB
/// of JSON. Every message this transport carried until now was far below it: an input event
/// is bytes, a video access unit is handed over as a dmabuf fd rather than inline, and
/// clipboard text is a line or two.
///
/// A pasted image is not. Measured: a 256 KB payload serializes to 349,596 bytes of JSON and
/// `sendmsg` fails outright with `EMSGSIZE`, and everything above it fails the same way. The
/// clipboard broker discarded that error, so the bytes never reached the clone, the daemon
/// never answered Mutter's pending `SelectionWrite`, and the pasting application sat on a pipe
/// nobody was going to write to until it gave up and pasted whatever it had. Slow, then
/// corrupt, from one dropped datagram.
///
/// **The frame.** A chunk is `\0RMC` then the message id, the chunk index and the chunk count,
/// each big-endian, then the slice. The magic starts with a NUL, which no JSON document can,
/// so a receiver tells a chunk from a whole message by looking at four bytes and old peers
/// are not silently fed a fragment they would parse as truth.
pub mod chunk {
    /// Marks a datagram as one piece of a larger message. A NUL first byte cannot start JSON.
    const MAGIC: [u8; 4] = [0, b'R', b'M', b'C'];
    /// `MAGIC` + id + index + count.
    const HEADER: usize = 4 + 8 + 4 + 4;

    /// Payload bytes per chunk.
    ///
    /// Fixed rather than derived from `SO_SNDBUF`, because the limit that matters is the
    /// smaller of the two peers' buffers and neither can see the other's. 64 KiB is comfortably
    /// under the 208 KB default at both ends, leaves room for a peer that has tuned its buffer
    /// down, and costs one datagram per 64 KiB: a 5 MB screenshot crosses in 80 of them.
    pub const CHUNK_BYTES: usize = 64 * 1024;

    /// Most bytes one reassembly will hold before it is abandoned.
    ///
    /// The sender is the control-server or a clone-daemon, not an attacker, but a peer that
    /// dies mid-message must not leave the other side holding its partial copy forever.
    pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

    /// The datagrams to send for one encoded message, in order.
    ///
    /// A message that already fits is returned whole and unwrapped, so the common case stays
    /// exactly the bytes it was before this existed.
    pub fn split(payload: &[u8], id: u64) -> Vec<Vec<u8>> {
        if payload.len() <= CHUNK_BYTES {
            return vec![payload.to_vec()];
        }
        let count = payload.len().div_ceil(CHUNK_BYTES);
        payload
            .chunks(CHUNK_BYTES)
            .enumerate()
            .map(|(i, slice)| {
                let mut out = Vec::with_capacity(HEADER + slice.len());
                out.extend_from_slice(&MAGIC);
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&(i as u32).to_be_bytes());
                out.extend_from_slice(&(count as u32).to_be_bytes());
                out.extend_from_slice(slice);
                out
            })
            .collect()
    }

    /// Whether this datagram is a chunk rather than a whole message.
    pub fn is_chunk(datagram: &[u8]) -> bool {
        datagram.len() >= HEADER && datagram[..4] == MAGIC
    }

    /// Puts a split message back together.
    ///
    /// One message at a time, which is all a connected `SOCK_SEQPACKET` needs: it delivers in
    /// order and never interleaves two `sendmsg` calls from one sender. A chunk that does not
    /// continue the message in progress starts a new one, so a peer that died mid-message
    /// costs the partial copy and nothing after it.
    #[derive(Debug, Default)]
    pub struct Reassembler {
        id: u64,
        next: u32,
        count: u32,
        buf: Vec<u8>,
    }

    impl Reassembler {
        /// Feed one datagram. `Some` when a whole message is ready, which for an unchunked
        /// datagram is immediately.
        pub fn push(&mut self, datagram: &[u8]) -> Option<Vec<u8>> {
            if !is_chunk(datagram) {
                return Some(datagram.to_vec());
            }
            let id = u64::from_be_bytes(datagram[4..12].try_into().ok()?);
            let index = u32::from_be_bytes(datagram[12..16].try_into().ok()?);
            let count = u32::from_be_bytes(datagram[16..20].try_into().ok()?);
            let body = &datagram[HEADER..];

            let continues = id == self.id && index == self.next && count == self.count;
            if !continues {
                if index != 0 {
                    self.buf.clear(); // A tail whose head we never saw is not a message.
                    self.count = 0;
                    return None;
                }
                self.id = id;
                self.count = count;
                self.buf.clear();
            }
            if self.buf.len() + body.len() > MAX_MESSAGE_BYTES {
                self.buf.clear();
                self.count = 0;
                return None;
            }
            self.buf.extend_from_slice(body);
            self.next = index + 1;
            (self.next == self.count).then(|| std::mem::take(&mut self.buf))
        }
    }
}

/// Base64 (de)serialization for binary blobs in JSON framing. Swap for raw bytes
/// if/when the framing goes binary.
pub(crate) mod serde_bytes_b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
            out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6 & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[(n & 63) as usize] as char
            } else {
                '='
            });
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
        let bytes: Vec<u8> = s
            .bytes()
            .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
            .collect();
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

    /// A daemon older than `fresh_session` sends `Hello` without it, and that has to read as
    /// "the holder was already running". The other way round would push a layout at every
    /// clone whose daemon reconnects, which is the fleet-wide swap the lazy layout removed.
    #[test]
    fn a_hello_without_the_fresh_flag_reads_as_an_existing_session() {
        let old: Hello = serde_json::from_str(r#"{"clone_id":"w1"}"#).unwrap();
        assert_eq!(
            old,
            Hello {
                clone_id: "w1".into(),
                fresh_session: false
            }
        );
        let new = Hello {
            clone_id: "w1".into(),
            fresh_session: true,
        };
        let back: Hello = serde_json::from_slice(&serde_json::to_vec(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }

    #[test]
    fn a_message_that_fits_is_sent_exactly_as_it_was() {
        // The common case must not gain a header: an input event and a cursor update are the
        // hot path, and every byte of envelope on them buys nothing.
        let small = b"{\"t\":\"input\"}".to_vec();
        let out = chunk::split(&small, 1);
        assert_eq!(out, vec![small.clone()]);
        assert!(!chunk::is_chunk(&small));
        assert_eq!(chunk::Reassembler::default().push(&small), Some(small));
    }

    #[test]
    fn an_oversized_message_splits_and_comes_back_identical() {
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let parts = chunk::split(&payload, 7);
        assert_eq!(parts.len(), payload.len().div_ceil(chunk::CHUNK_BYTES));
        assert!(
            parts.iter().all(|p| chunk::is_chunk(p)),
            "every piece is marked"
        );
        assert!(parts.iter().all(|p| p.len() <= chunk::CHUNK_BYTES + 20));

        let mut join = chunk::Reassembler::default();
        let last = parts.len() - 1;
        for (i, part) in parts.iter().enumerate() {
            let got = join.push(part);
            assert_eq!(got.is_some(), i == last, "only the last chunk completes it");
            if let Some(whole) = got {
                assert_eq!(whole, payload);
            }
        }
    }

    #[test]
    fn a_message_cut_short_never_becomes_a_shorter_one() {
        // A sender that gives up mid-message must not leave the receiver holding a prefix it
        // will later staple to something else. The next message starts at chunk 0, and that is
        // what resets it.
        let first: Vec<u8> = vec![b'a'; 200_000];
        let second: Vec<u8> = vec![b'b'; 200_000];
        let mut join = chunk::Reassembler::default();
        for part in chunk::split(&first, 1).iter().take(2) {
            assert_eq!(join.push(part), None);
        }
        let mut done = None;
        for part in chunk::split(&second, 2) {
            done = join.push(&part).or(done);
        }
        assert_eq!(
            done,
            Some(second),
            "the abandoned prefix is gone, not prepended"
        );
    }

    #[test]
    fn a_tail_with_no_head_is_dropped_rather_than_kept() {
        // What a receiver sees when it connects mid-message, or when chunk 0 was the one the
        // sender failed on. There is no message here, and inventing a short one would hand a
        // truncated image to whoever pasted.
        let payload: Vec<u8> = vec![b'z'; 200_000];
        let parts = chunk::split(&payload, 3);
        let mut join = chunk::Reassembler::default();
        assert_eq!(join.push(&parts[1]), None);
        assert_eq!(join.push(&parts[2]), None);
        // And it recovers on the next whole message.
        assert_eq!(join.push(b"{}"), Some(b"{}".to_vec()));
    }

    #[test]
    fn input_msg_tagged_roundtrip() {
        let m = InputMsg::PointerMove {
            monitor_id: 0,
            x: 12.0,
            y: 34.0,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"kind\":\"pointer_move\""));
        assert_eq!(serde_json::from_str::<InputMsg>(&s).unwrap(), m);

        let c = InputMsg::AxisContinuous {
            dx: 1.5,
            dy: -3.0,
            flags: axis_flags::SOURCE_FINGER,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"kind\":\"axis_continuous\""), "{s}");
        assert_eq!(serde_json::from_str::<InputMsg>(&s).unwrap(), c);
    }

    #[test]
    fn cursor_meta_hidden_roundtrip() {
        // Hide transition: flag serialized, roundtrips.
        let c = CursorMeta {
            monitor_id: 1,
            x: 5,
            y: 6,
            shape: None,
            warp: false,
            hidden: true,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"hidden\":true"), "{s}");
        assert_eq!(serde_json::from_str::<CursorMeta>(&s).unwrap(), c);

        // Visible / position-only: flag omitted on the wire, defaults on decode
        // (old daemons never send it).
        let c = CursorMeta {
            monitor_id: 1,
            x: 5,
            y: 6,
            shape: None,
            warp: false,
            hidden: false,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("hidden"), "{s}");
        let old = r#"{"monitor_id":1,"x":5,"y":6}"#;
        assert_eq!(serde_json::from_str::<CursorMeta>(old).unwrap(), c);
    }

    #[test]
    fn clipboard_msg_tags() {
        let offer = ClipboardMsg::Offer(ClipboardOffer {
            serial: 1,
            mime_types: vec!["text/html".into()],
        });
        let s = serde_json::to_string(&offer).unwrap();
        assert!(s.contains("\"k\":\"offer\""), "{s}");
        assert_eq!(serde_json::from_str::<ClipboardMsg>(&s).unwrap(), offer);

        let req = DaemonMsg::ClipboardRequest(ClipboardRequest {
            serial: 2,
            mime_type: "image/png".into(),
        });
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"t\":\"clipboard_request\""), "{s}");
        assert_eq!(serde_json::from_str::<DaemonMsg>(&s).unwrap(), req);
    }

    #[test]
    fn base64_roundtrips() {
        for case in [
            vec![],
            vec![0u8],
            vec![1, 2, 3],
            (0u8..=255).collect::<Vec<_>>(),
        ] {
            let data = ClipboardData {
                serial: 1,
                mime_type: "x".into(),
                bytes: case.clone(),
            };
            let s = serde_json::to_string(&data).unwrap();
            let back: ClipboardData = serde_json::from_str(&s).unwrap();
            assert_eq!(back.bytes, case);
        }
        // Every length modulo 4, so both padded tails are exercised at a size where an
        // off-by-one in the tail would show as a corrupt image rather than a failed decode.
        for len in 4_093..4_100 {
            let case: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let data = ClipboardData {
                serial: 1,
                mime_type: "image/png".into(),
                bytes: case.clone(),
            };
            let s = serde_json::to_string(&data).unwrap();
            let back: ClipboardData = serde_json::from_str(&s).unwrap();
            assert_eq!(back.bytes, case, "length {len}");
        }
    }

    #[test]
    fn server_msg_set_monitors_tag() {
        use crate::control::MonitorSpec;
        let m = ServerMsg::SetMonitors {
            monitors: vec![MonitorSpec {
                width: 1920,
                height: 1080,
                x: 0,
                y: 0,
                primary: true,
            }],
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["t"], "set_monitors");
        assert_eq!(v["monitors"][0]["width"], 1920);
        let back: ServerMsg = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
    }

    // Forward compatibility: an unknown `t` tag must deserialize to `Unknown` (Ok), NOT an
    // Err — so an old peer ignores a newer variant instead of treating the decode as a
    // fatal socket error and dropping the connection.
    #[test]
    fn server_msg_unknown_variant_is_ok_not_err() {
        let back: ServerMsg = serde_json::from_str(r#"{"t":"some_future_variant","foo":42}"#)
            .expect("unknown tag → Ok");
        assert_eq!(back, ServerMsg::Unknown);
        // A known variant still round-trips.
        let ack: ServerMsg = serde_json::from_str(r#"{"t":"ack","monitor_id":1,"seq":7}"#).unwrap();
        assert!(matches!(ack, ServerMsg::Ack(_)));
    }

    #[test]
    fn daemon_msg_unknown_variant_is_ok_not_err() {
        let back: DaemonMsg = serde_json::from_str(r#"{"t":"some_future_variant","foo":42}"#)
            .expect("unknown tag → Ok");
        assert_eq!(back, DaemonMsg::Unknown);
        let hello: DaemonMsg = serde_json::from_str(r#"{"t":"hello","clone_id":"c1"}"#).unwrap();
        assert!(matches!(hello, DaemonMsg::Hello(_)));
    }
}
