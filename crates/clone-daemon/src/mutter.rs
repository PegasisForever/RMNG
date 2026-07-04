//! Mutter's private session-bus APIs (the same `gnome-remote-desktop` uses):
//! ScreenCast (RecordVirtual → dmabuf streams) + RemoteDesktop (input inject).
//! zbus proxies re-expressed from the reference in `computer-use/src/mutter.rs`.
//!
//! Sessions are tied to the creating D-Bus connection — Mutter destroys them when
//! the creator drops off the bus — so the daemon keeps one long-lived connection.

use std::collections::HashMap;

use anyhow::{Context, Result};
use futures::StreamExt;
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, Value};

/// cursor-mode: 0 hidden, 1 embedded (composited into the frame), 2 metadata.
/// v1 uses EMBEDDED (PoC-validated); local-cursor METADATA is a later refinement.
pub const CURSOR_MODE_EMBEDDED: u32 = 1;
/// cursor-mode METADATA: the cursor is delivered out-of-band as `SPA_META_Cursor`
/// (position + shape bitmap) on the PipeWire buffers, NOT composited into the frame.
/// Required by the raw-PipeWire capture (`capture_pw`) which draws the cursor
/// client-side. GStreamer's `pipewiresrc` can't surface this metadata.
pub const CURSOR_MODE_METADATA: u32 = 2;

#[proxy(
    interface = "org.gnome.Mutter.RemoteDesktop",
    default_service = "org.gnome.Mutter.RemoteDesktop",
    default_path = "/org/gnome/Mutter/RemoteDesktop"
)]
pub trait RemoteDesktop {
    fn create_session(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.gnome.Mutter.RemoteDesktop.Session",
    default_service = "org.gnome.Mutter.RemoteDesktop"
)]
pub trait RemoteDesktopSession {
    fn start(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    // Input injection: `#[zbus(no_reply)]` sets the NO_REPLY_EXPECTED flag so the call
    // returns as soon as the message is on the bus, instead of awaiting Mutter's empty
    // method-return. These run one-at-a-time off the inject channel (main.rs), so awaiting
    // each reply serialized a burst of motion behind a local D-Bus round-trip apiece;
    // fire-and-forget lets them pipeline. D-Bus preserves per-connection message order, so
    // button/motion/key sequencing is unchanged. We never use the returned `()`; the
    // tradeoff is losing per-call error surfacing, which for input inject is non-actionable.
    #[zbus(no_reply)]
    fn notify_keyboard_keysym(&self, keysym: u32, state: bool) -> zbus::Result<()>;
    /// Inject a raw evdev keycode (physical-key identity, for games).
    #[zbus(no_reply)]
    fn notify_keyboard_keycode(&self, keycode: u32, state: bool) -> zbus::Result<()>;
    #[zbus(no_reply)]
    fn notify_pointer_button(&self, button: i32, state: bool) -> zbus::Result<()>;
    #[zbus(no_reply)]
    fn notify_pointer_axis_discrete(&self, axis: u32, steps: i32) -> zbus::Result<()>;
    /// Relative (unaccelerated) pointer motion — for pointer-lock / games.
    #[zbus(no_reply)]
    fn notify_pointer_motion_relative(&self, dx: f64, dy: f64) -> zbus::Result<()>;
    #[zbus(no_reply)]
    fn notify_pointer_motion_absolute(&self, stream: &str, x: f64, y: f64) -> zbus::Result<()>;

    // --- clipboard (rich + lazy; we use it for text) ---
    fn enable_clipboard(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    fn disable_clipboard(&self) -> zbus::Result<()>;
    fn set_selection(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    /// We own the selection + a peer is pasting: returns an fd to write the data to.
    fn selection_write(&self, serial: u32) -> zbus::Result<zbus::zvariant::OwnedFd>;
    fn selection_write_done(&self, serial: u32, success: bool) -> zbus::Result<()>;
    /// Read the current selection's data for `mime_type`: returns an fd to read from.
    fn selection_read(&self, mime_type: &str) -> zbus::Result<zbus::zvariant::OwnedFd>;

    #[zbus(property)]
    fn session_id(&self) -> zbus::Result<String>;
    #[zbus(signal)]
    fn closed(&self) -> zbus::Result<()>;
    /// The clone's selection owner changed (a clone app copied something).
    #[zbus(signal)]
    fn selection_owner_changed(&self, options: HashMap<String, zbus::zvariant::OwnedValue>) -> zbus::Result<()>;
    /// A clone app is pasting our owned selection; we must write `serial`'s fd.
    #[zbus(signal)]
    fn selection_transfer(&self, mime_type: String, serial: u32) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.gnome.Mutter.ScreenCast",
    default_service = "org.gnome.Mutter.ScreenCast",
    default_path = "/org/gnome/Mutter/ScreenCast"
)]
pub trait ScreenCast {
    fn create_session(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.gnome.Mutter.ScreenCast.Session",
    default_service = "org.gnome.Mutter.ScreenCast"
)]
pub trait ScreenCastSession {
    fn start(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn record_virtual(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.gnome.Mutter.ScreenCast.Stream",
    default_service = "org.gnome.Mutter.ScreenCast"
)]
pub trait ScreenCastStream {
    #[zbus(signal)]
    fn pipe_wire_stream_added(&self, node_id: u32) -> zbus::Result<()>;
}

/// One held virtual monitor: its ScreenCast stream + PipeWire node + size.
#[derive(Debug, Clone)]
pub struct VirtualMonitor {
    pub monitor_id: u32,
    pub stream_path: String,
    pub node_id: u32,
    pub width: u32,
    pub height: u32,
}

/// A live Mutter session: the RemoteDesktop session (for input) + the held
/// virtual monitors (for capture). Keep it alive for the process's lifetime.
pub struct Session {
    pub conn: zbus::Connection,
    pub rd: RemoteDesktopSessionProxy<'static>,
    pub monitors: Vec<VirtualMonitor>,
}

impl Session {
    /// Stop this Mutter session — tears down the RemoteDesktop session, its linked
    /// ScreenCast session, and all virtual monitors. Used by the make-before-break swap
    /// to drop the OLD session after the new one is capturing (Phase 0: apps survive).
    pub async fn stop(&self) -> zbus::Result<()> {
        self.rd.stop().await
    }
}

fn build_modes(w: u32, h: u32) -> Vec<HashMap<String, Value<'static>>> {
    let mut m = HashMap::new();
    m.insert("size".to_string(), Value::new((w, h)));
    m.insert("refresh-rate".to_string(), Value::new(60.0_f64));
    // Mutter rejects a mode set with no preferred mode ("No preferred modes").
    m.insert("is-preferred".to_string(), Value::new(true));
    vec![m]
}

/// Create the RemoteDesktop + ScreenCast sessions, RecordVirtual one monitor per
/// requested size, start them, and resolve each stream's PipeWire node id, with an
/// explicit `cursor-mode` (see `CURSOR_MODE_*`). Pass `CURSOR_MODE_METADATA` to receive
/// the cursor out-of-band via `SPA_META_Cursor` on the PipeWire buffers (the raw-PW
/// client-cursor path); `CURSOR_MODE_EMBEDDED` composites it into the frame instead.
pub async fn setup_with_cursor_mode(sizes: &[(u32, u32)], cursor_mode: u32) -> Result<Session> {
    let conn = zbus::Connection::session().await.context("session bus")?;

    let rd = RemoteDesktopProxy::new(&conn).await?;
    let rd_path = rd.create_session().await.context("RemoteDesktop.CreateSession")?;
    let rd_session = RemoteDesktopSessionProxy::builder(&conn)
        .path(rd_path.clone())?
        .build()
        .await?;
    let session_id = rd_session.session_id().await.context("reading SessionId")?;

    let sc = ScreenCastProxy::new(&conn).await?;
    let mut sc_props: HashMap<&str, Value<'_>> = HashMap::new();
    sc_props.insert("remote-desktop-session-id", Value::from(session_id));
    let sc_path = sc.create_session(sc_props).await.context("ScreenCast.CreateSession")?;
    let sc_session = ScreenCastSessionProxy::builder(&conn).path(sc_path.clone())?.build().await?;

    // RecordVirtual each monitor; subscribe to its node-added signal before Start.
    let mut pending = Vec::new();
    for (i, (w, h)) in sizes.iter().enumerate() {
        let mut props: HashMap<&str, Value<'_>> = HashMap::new();
        props.insert("cursor-mode", Value::from(cursor_mode));
        props.insert("is-platform", Value::from(true));
        props.insert("modes", Value::new(build_modes(*w, *h)));
        let stream_path = sc_session.record_virtual(props).await.context("RecordVirtual")?;
        let stream = ScreenCastStreamProxy::builder(&conn).path(stream_path.clone())?.build().await?;
        let added = stream.receive_pipe_wire_stream_added().await?;
        pending.push((i as u32, stream_path.to_string(), added, *w, *h));
    }

    // Starting the RemoteDesktop session starts the linked ScreenCast session too
    // (calling ScreenCast.Session.Start directly errors "Must be started from
    // remote desktop session"). PipeWireStreamAdded fires off this Start.
    rd_session.start().await.context("RemoteDesktop.Session.Start")?;

    // Resolve node ids (the signal fires after Start).
    let mut monitors = Vec::new();
    for (monitor_id, stream_path, mut added, width, height) in pending {
        let sig = added.next().await.context("waiting for PipeWireStreamAdded")?;
        let node_id = sig.args().context("PipeWireStreamAdded args")?.node_id;
        tracing::info!(monitor_id, node_id, width, height, "virtual monitor ready");
        monitors.push(VirtualMonitor { monitor_id, stream_path, node_id, width, height });
    }

    // Leak the session proxies' lifetime to 'static by keeping `conn` in Session.
    Ok(Session { conn, rd: rd_session, monitors })
}
