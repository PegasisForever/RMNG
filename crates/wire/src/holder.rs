//! The clone-local protocol between `rmng-clone-daemon` and the session holder
//! (`rmng-clone-daemon --session-holder`), spoken over a `SOCK_SEQPACKET` socket in the
//! clone's `XDG_RUNTIME_DIR`.
//!
//! **Why the two are separate processes.** Mutter destroys a RemoteDesktop session when the
//! D-Bus connection that created it drops, taking every virtual monitor with it, and
//! gnome-shell remaps every window when the monitor set empties. The daemon restarts on
//! every payload push, so the session lives in a process that does not: the holder. It owns
//! the bus connection, the sessions, the virtual monitors, input injection, and the
//! clipboard. The daemon keeps everything that is not session-bound (capture by PipeWire
//! node id, encode, ship, the MCP) and reconnects after each restart.
//!
//! **Why input and clipboard have to cross.** Mutter answers session methods only for the
//! connection that created the session: calling `NotifyPointerMotionRelative` from any other
//! connection returns `org.freedesktop.DBus.Error.AccessDenied`. `org.gnome.Shell.Eval` has
//! no such check, so the window tools stay in the daemon on its own connection.
//!
//! No file descriptor crosses this socket. Capture connects to the holder's PipeWire node by
//! id from its own process (verified: a separate client negotiates the same DMABuf caps), and
//! the clipboard travels as bytes because the holder reads and writes Mutter's selection fds
//! itself.

use serde::{Deserialize, Serialize};

use crate::control::MonitorSpec;
use crate::socket::{ClipboardData, ClipboardOffer, ClipboardRequest, InputMsg, MonitorPlacement};

/// Protocol version, and the only thing that makes a running holder restart.
///
/// The daemon sends it in [`ToHolder::Hello`] and restarts the holder on a mismatch, which
/// costs one window reset on that release and nothing on the others.
///
/// Bump it for two kinds of change. One is a message whose meaning changed, though not merely
/// one that was added: an added variant already decodes to `Unknown` on the older side. The
/// other is any fix to the holder half that has to reach clones that are already running,
/// because a payload push replaces the binary on disk and leaves the old process running it.
/// Without a bump such a fix waits for the clone to restart, which may be never.
/// Version 2 carries the cursor fix: the holder read its missing `RMNG_SOCKET` as "this is the
/// capture self-test" and built every session with the cursor composited into the frame, so a
/// running holder has to be replaced for a viewer to get a real pointer back.
pub const PROTO_VERSION: u32 = 2;

/// Where the holder binds and the daemon connects.
///
/// `RMNG_HOLDER_SOCKET` overrides it (tests, and a second holder on one desktop).
pub fn socket_path() -> String {
    if let Ok(p) = std::env::var("RMNG_HOLDER_SOCKET") {
        return p;
    }
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    format!("{dir}/rmng-session-holder.sock")
}

/// One virtual monitor the holder is holding open.
///
/// `monitor_id` is the slot index, matching the layout the server pushed. `node_id` is what
/// the daemon's capture connects to. The Mutter stream path is deliberately absent: only the
/// holder may use it, for absolute pointer motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderMonitor {
    pub monitor_id: u32,
    pub node_id: u32,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}

impl HolderMonitor {
    /// The placement to report to the control-server, which the viewer routes drags against.
    pub fn placement(&self) -> MonitorPlacement {
        MonitorPlacement {
            id: self.monitor_id,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            primary: self.primary,
        }
    }
}

/// daemon → holder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToHolder {
    /// First message on connect. The holder answers [`FromHolder::HelloOk`] with the monitor
    /// set it already holds, whether or not this is the daemon's first run.
    Hello { proto: u32 },
    /// Inject one event. Fire-and-forget: `SOCK_SEQPACKET` preserves order on a connection,
    /// so a press still lands before its release.
    Input(InputMsg),
    /// Apply a new monitor layout, make-before-break. Answered by [`FromHolder::Monitors`].
    SetLayout { monitors: Vec<MonitorSpec> },
    /// Capture is running on every node in `generation`, so the holder may drop the old
    /// session. Second step of the swap handshake.
    CaptureReady { generation: u64 },
    /// The operator's machine put something on the clipboard: offer it to the clone.
    ClipboardOffer(ClipboardOffer),
    /// Bytes the clone asked for, answering an earlier [`FromHolder::ClipboardRequest`].
    ClipboardData(ClipboardData),
    /// Fetch the bytes a clone app is offering, for a paste on the operator's machine.
    ClipboardRequest(ClipboardRequest),
    /// A `t` tag this build does not know. See [`crate::socket::ServerMsg::Unknown`].
    #[serde(other)]
    Unknown,
}

/// holder → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromHolder {
    /// Answer to [`ToHolder::Hello`]. `proto` is the holder's version, which the daemon
    /// compares with [`PROTO_VERSION`] before trusting anything else in this message.
    HelloOk {
        proto: u32,
        generation: u64,
        monitors: Vec<HolderMonitor>,
    },
    /// A new monitor set exists and is capturable. Sent for a pushed layout and for a session
    /// the holder had to rebuild (gnome-shell restart). First step of the swap handshake: the
    /// daemon starts capture on these nodes, then answers [`ToHolder::CaptureReady`].
    Monitors {
        generation: u64,
        monitors: Vec<HolderMonitor>,
    },
    /// The old session is gone and the layout is applied, so the daemon may tear down the
    /// captures of every earlier generation. Third step of the swap handshake.
    SwapDone { generation: u64 },
    /// A clone app put something on its clipboard.
    ClipboardOffer(ClipboardOffer),
    /// A clone app is pasting a remote selection: the daemon asks the broker for the bytes.
    ClipboardRequest(ClipboardRequest),
    /// Bytes for an earlier [`ToHolder::ClipboardRequest`].
    ClipboardData(ClipboardData),
    /// A `t` tag this build does not know.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let to = ToHolder::Input(InputMsg::Button { button: 0x110, pressed: true });
        let back: ToHolder = serde_json::from_slice(&serde_json::to_vec(&to).unwrap()).unwrap();
        assert_eq!(to, back);

        let from = FromHolder::Monitors {
            generation: 7,
            monitors: vec![HolderMonitor {
                monitor_id: 1,
                node_id: 64,
                width: 1920,
                height: 1080,
                x: 1920,
                y: 0,
                primary: false,
            }],
        };
        let back: FromHolder =
            serde_json::from_slice(&serde_json::to_vec(&from).unwrap()).unwrap();
        assert_eq!(from, back);
    }

    /// A variant this build does not know decodes to `Unknown` instead of failing, so one
    /// side running a newer protocol cannot kill the other's reader loop before the version
    /// handshake gets a chance to restart the holder.
    #[test]
    fn an_unknown_variant_decodes_instead_of_failing() {
        let to: ToHolder = serde_json::from_str(r#"{"t":"teleport","x":1}"#).unwrap();
        assert_eq!(to, ToHolder::Unknown);
        let from: FromHolder = serde_json::from_str(r#"{"t":"teleport","x":1}"#).unwrap();
        assert_eq!(from, FromHolder::Unknown);
    }

    #[test]
    fn placement_carries_the_slot_index() {
        let m = HolderMonitor {
            monitor_id: 2,
            node_id: 9,
            width: 1280,
            height: 720,
            x: 3840,
            y: 100,
            primary: true,
        };
        let p = m.placement();
        assert_eq!((p.id, p.x, p.y, p.width, p.height, p.primary), (2, 3840, 100, 1280, 720, true));
    }

    #[test]
    fn the_socket_path_follows_the_runtime_dir() {
        // Both env vars are read at call time, so set them here rather than at process start.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/4242");
            std::env::remove_var("RMNG_HOLDER_SOCKET");
        }
        assert_eq!(socket_path(), "/run/user/4242/rmng-session-holder.sock");
        unsafe { std::env::set_var("RMNG_HOLDER_SOCKET", "/tmp/h.sock") };
        assert_eq!(socket_path(), "/tmp/h.sock");
        unsafe { std::env::remove_var("RMNG_HOLDER_SOCKET") };
    }
}
