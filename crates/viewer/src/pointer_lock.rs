//! Wayland pointer lock for cursor-grabbing remote apps (games, e.g. Minecraft).
//!
//! GTK4 exposes no pointer-lock API, so this drives the Wayland
//! `pointer-constraints` + `relative-pointer` protocols directly over GTK's own
//! Wayland connection (via gdk4-wayland). While engaged the local pointer is
//! locked in place and unbounded relative-motion deltas are forwarded to the
//! control-server as `InputMsg::PointerRelative`, which the clone-daemon injects
//! via Mutter `NotifyPointerMotionRelative` — the only motion a server-side
//! locked pointer (a game) accepts.
//!
//! Wayland-only: on X11 (or with `RMNG_NO_POINTER_LOCK=1`) construction
//! returns `None` and the viewer keeps its absolute-pointer behaviour.
//! Ported from `../../gtk/src/pointer_lock.rs`.

use std::cell::RefCell;
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use gdk4_wayland::prelude::*;
use gdk4_wayland::{WaylandDisplay, WaylandSeat};
use gtk4::gdk;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::ZwpLockedPointerV1;
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::{
    Lifetime, ZwpPointerConstraintsV1,
};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1;
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::{
    Event as RelEvent, ZwpRelativePointerV1,
};

/// The viewer's input write half (port-1 socket); shared with the GTK thread.
type Writer = Arc<Mutex<Option<TcpStream>>>;

/// Frame one input message to the server: `[0u8][u32be len][json]` (tag 0 = input).
fn send_relative(writer: &Writer, dx: f64, dy: f64) {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n % 100 == 0 {
        tracing::debug!("pointer-lock: relative motion #{n} dx={dx} dy={dy} (writer {})",
            if writer.lock().unwrap().is_some() { "connected" } else { "MISSING" });
    }
    let json = format!(r#"{{"kind":"pointer_relative","dx":{dx},"dy":{dy}}}"#);
    if let Some(g) = writer.lock().unwrap().as_mut() {
        let hdr = (json.len() as u32).to_be_bytes();
        let _ = g
            .write_all(&[0u8])
            .and_then(|_| g.write_all(&hdr))
            .and_then(|_| g.write_all(json.as_bytes()));
    }
}

/// State owned by the background Wayland dispatch thread: accumulates sub-pixel
/// relative motion and forwards whole-unit deltas to the control-server.
struct State {
    writer: Writer,
    rem_x: f64,
    rem_y: f64,
}

pub struct PointerLock {
    conn: Connection,
    qh: QueueHandle<State>,
    constraints: ZwpPointerConstraintsV1,
    rel_mgr: ZwpRelativePointerManagerV1,
    /// Our own (passive) seat pointer used as the constraint/relative-pointer
    /// target; GTK's own wl_pointer keeps driving normal input and the cursor.
    pointer: WlPointer,
    locked: RefCell<Option<ZwpLockedPointerV1>>,
    rel: RefCell<Option<ZwpRelativePointerV1>>,
    /// wl_surface id currently locked, so `engage` can re-target on focus moves.
    current: RefCell<Option<ObjectId>>,
}

impl PointerLock {
    /// Set up pointer-lock over GTK's Wayland connection. Returns `None` on a
    /// non-Wayland session, when the compositor lacks the protocols, or when
    /// `RMNG_NO_POINTER_LOCK=1`.
    pub fn new(display: &gdk::Display, writer: Writer) -> Option<Self> {
        if std::env::var_os("RMNG_NO_POINTER_LOCK").is_some() {
            return None;
        }
        // Confirms a Wayland session; also forces gdk4-wayland to create and
        // cache the shared wayland-client Connection on the display.
        let _wl_display = display.downcast_ref::<WaylandDisplay>()?;
        let seat = display.default_seat()?;
        let wl_seat = seat.downcast_ref::<WaylandSeat>()?.wl_seat()?;
        // Reuse GTK's own Wayland backend so our objects share its connection.
        let conn = Connection::from_backend(wl_seat.backend().upgrade()?);

        let (globals, queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        // mutter exposes both; a compositor without them disables the feature.
        let constraints: ZwpPointerConstraintsV1 = globals.bind(&qh, 1..=1, ()).ok()?;
        let rel_mgr: ZwpRelativePointerManagerV1 = globals.bind(&qh, 1..=1, ()).ok()?;
        let pointer = wl_seat.get_pointer(&qh, ());

        let mut state = State { writer, rem_x: 0.0, rem_y: 0.0 };
        let mut queue = queue;
        std::thread::Builder::new()
            .name("rmng-wl-relptr".into())
            .spawn(move || {
                // Cooperative multi-reader dispatch sharing GTK's display fd.
                while queue.blocking_dispatch(&mut state).is_ok() {}
            })
            .ok()?;
        let _ = conn.flush();
        tracing::info!("Wayland pointer lock ready (Ctrl+Alt+G toggles, Ctrl+Alt+P releases)");

        Some(PointerLock {
            conn,
            qh,
            constraints,
            rel_mgr,
            pointer,
            locked: RefCell::new(None),
            rel: RefCell::new(None),
            current: RefCell::new(None),
        })
    }

    pub fn is_engaged(&self) -> bool {
        self.locked.borrow().is_some()
    }

    /// Lock the pointer to `surface` and start relaying relative motion.
    /// Idempotent for the same surface; re-targets when focus moves to another.
    pub fn engage(&self, surface: &gdk::Surface) {
        let Some(wl_surface) =
            surface.downcast_ref::<gdk4_wayland::WaylandSurface>().and_then(|s| s.wl_surface())
        else {
            return;
        };
        let id = wl_surface.id();
        if self.current.borrow().as_ref() == Some(&id) {
            tracing::debug!("pointer-lock engage: already locked to surface {id:?}, no-op");
            return;
        }
        self.destroy_objects();
        tracing::info!("pointer-lock engage: lock_pointer on surface {id:?}");
        let locked =
            self.constraints.lock_pointer(&wl_surface, &self.pointer, None, Lifetime::Persistent, &self.qh, ());
        let rel = self.rel_mgr.get_relative_pointer(&self.pointer, &self.qh, ());
        *self.locked.borrow_mut() = Some(locked);
        *self.rel.borrow_mut() = Some(rel);
        *self.current.borrow_mut() = Some(id);
        let _ = self.conn.flush();
    }

    /// Release the lock and stop relaying motion.
    pub fn release(&self) {
        if self.current.borrow().is_none() {
            return;
        }
        tracing::info!("pointer-lock release");
        self.destroy_objects();
        *self.current.borrow_mut() = None;
        let _ = self.conn.flush();
    }

    fn destroy_objects(&self) {
        if let Some(l) = self.locked.borrow_mut().take() {
            l.destroy();
        }
        if let Some(r) = self.rel.borrow_mut().take() {
            r.destroy();
        }
    }
}

// The registry + manager globals and our passive pointer produce no events we
// act on; only the relative pointer does.
impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

macro_rules! ignore_events {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

ignore_events!(ZwpPointerConstraintsV1);
ignore_events!(ZwpRelativePointerManagerV1);

// Log lock activation/deactivation: mutter only pins the cursor once it sends
// `locked`; a lock request that never activates is the primary failure mode.
impl Dispatch<ZwpLockedPointerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpLockedPointerV1,
        event: <ZwpLockedPointerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::Event as LE;
        match event {
            LE::Locked => tracing::info!("pointer-lock ACTIVATED by compositor"),
            LE::Unlocked => tracing::info!("pointer-lock DEACTIVATED by compositor"),
            _ => {}
        }
    }
}

// Our passive pointer's enter/leave shows the pointer-focus state mutter uses
// to decide constraint activation.
impl Dispatch<WlPointer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlPointer,
        event: <WlPointer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_pointer::Event as PE;
        match event {
            PE::Enter { surface, .. } => {
                tracing::debug!("pointer-lock: seat pointer entered surface {:?}", surface.id());
            }
            PE::Leave { surface, .. } => {
                tracing::debug!("pointer-lock: seat pointer left surface {:?}", surface.id());
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpRelativePointerV1,
        event: RelEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Unaccelerated deltas: the remote (mutter, then the game) applies its own
        // pointer acceleration, so forwarding raw motion avoids stacking a second
        // acceleration curve on top.
        let RelEvent::RelativeMotion { dx_unaccel, dy_unaccel, .. } = event else {
            return;
        };
        state.rem_x += dx_unaccel;
        state.rem_y += dy_unaccel;
        let ix = state.rem_x.trunc();
        let iy = state.rem_y.trunc();
        state.rem_x -= ix;
        state.rem_y -= iy;
        if ix != 0.0 || iy != 0.0 {
            send_relative(&state.writer, ix, iy);
        }
    }
}
