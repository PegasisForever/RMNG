//! Standalone pointer-lock probe: reproduces the viewer's exact Wayland
//! pointer-lock sequence (second wl_pointer + zwp_pointer_constraints lock)
//! with FULL event logging, so we can see whether mutter ever activates the
//! lock ("locked" event) and whether relative motion flows.
//!
//! Timeline: t=0 fullscreen window; t=1s lock_pointer on the window surface
//! (same call the viewer's Ctrl+Alt+G makes); t=12s release; t=13s quit.
#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}
#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::Duration;

    use gdk4_wayland::prelude::*;
    use gdk4_wayland::{WaylandDisplay, WaylandSeat};
    use gtk4::prelude::*;
    use gtk4::{gdk, glib};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_pointer::{Event as PtrEvent, WlPointer};
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::{
        Event as LockEvent, ZwpLockedPointerV1,
    };
    use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::{
        Lifetime, ZwpPointerConstraintsV1,
    };
    use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1;
    use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::{
        Event as RelEvent, ZwpRelativePointerV1,
    };

    fn ts() -> String {
        format!("{:>8.3}", START.elapsed().as_secs_f64())
    }
    static START: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);

    struct State {
        rel_count: u32,
        rel_sum: (f64, f64),
    }

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

    impl Dispatch<ZwpLockedPointerV1, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpLockedPointerV1,
            event: LockEvent,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                LockEvent::Locked => eprintln!("[{}] *** LOCKED event from compositor ***", ts()),
                LockEvent::Unlocked => {
                    eprintln!("[{}] *** UNLOCKED event from compositor ***", ts())
                }
                _ => eprintln!("[{}] locked-pointer: other event", ts()),
            }
        }
    }

    impl Dispatch<WlPointer, ()> for State {
        fn event(
            _: &mut Self,
            _: &WlPointer,
            event: PtrEvent,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                PtrEvent::Enter { serial, surface, surface_x, surface_y } => eprintln!(
                    "[{}] our wl_pointer ENTER serial={serial} surface={:?} at ({surface_x:.0},{surface_y:.0})",
                    ts(),
                    surface.id()
                ),
                PtrEvent::Leave { serial, surface } => eprintln!(
                    "[{}] our wl_pointer LEAVE serial={serial} surface={:?}",
                    ts(),
                    surface.id()
                ),
                PtrEvent::Motion { .. } | PtrEvent::Frame => {}
                other => eprintln!("[{}] our wl_pointer event: {other:?}", ts()),
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
            let RelEvent::RelativeMotion { dx_unaccel, dy_unaccel, .. } = event else {
                return;
            };
            state.rel_count += 1;
            state.rel_sum.0 += dx_unaccel;
            state.rel_sum.1 += dy_unaccel;
            if state.rel_count % 20 == 1 {
                eprintln!(
                    "[{}] relative motion #{}: last=({dx_unaccel:.1},{dy_unaccel:.1}) sum=({:.0},{:.0})",
                    ts(),
                    state.rel_count,
                    state.rel_sum.0,
                    state.rel_sum.1
                );
            }
        }
    }

    pub fn run() {
        let _ = &*START;
        let app = gtk4::Application::new(Some("site.pegasis.rmng.locktest"), Default::default());
        app.connect_activate(|app| {
            let window = gtk4::ApplicationWindow::new(app);
            window.set_title(Some("RMNG pointer-lock probe (auto-closes in 13s)"));
            let label = gtk4::Label::new(Some("pointer-lock probe — auto-closes"));
            window.set_child(Some(&label));
            window.fullscreen();
            window.present();

            let display = gdk::Display::default().expect("display");
            let Some(_wl_display) = display.downcast_ref::<WaylandDisplay>() else {
                eprintln!("not a Wayland display; aborting");
                app.quit();
                return;
            };
            let seat = display.default_seat().expect("seat");
            let wl_seat = seat.downcast_ref::<WaylandSeat>().unwrap().wl_seat().expect("wl_seat");
            let conn = Connection::from_backend(wl_seat.backend().upgrade().expect("backend"));

            let (globals, queue) = registry_queue_init::<State>(&conn).expect("registry");
            let qh = queue.handle();
            let constraints: ZwpPointerConstraintsV1 =
                globals.bind(&qh, 1..=1, ()).expect("pointer-constraints global");
            let rel_mgr: ZwpRelativePointerManagerV1 =
                globals.bind(&qh, 1..=1, ()).expect("relative-pointer global");
            let pointer = wl_seat.get_pointer(&qh, ());
            eprintln!("[{}] globals bound; second wl_pointer {:?}", ts(), pointer.id());

            let mut state = State { rel_count: 0, rel_sum: (0.0, 0.0) };
            let mut queue = queue;
            std::thread::spawn(move || while queue.blocking_dispatch(&mut state).is_ok() {});
            let _ = conn.flush();

            // t=1s: the viewer's exact engage() sequence on the window surface.
            let (win2, conn2, qh2) = (window.clone(), conn.clone(), qh.clone());
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(1000), move || {
                let surface = win2.surface().expect("gdk surface");
                let wl_surface = surface
                    .downcast_ref::<gdk4_wayland::WaylandSurface>()
                    .and_then(|s| s.wl_surface())
                    .expect("wl_surface");
                eprintln!("[{}] engaging lock on surface {:?}", ts(), wl_surface.id());
                let locked = constraints.lock_pointer(
                    &wl_surface,
                    &pointer,
                    None,
                    Lifetime::Persistent,
                    &qh2,
                    (),
                );
                let _rel = rel_mgr.get_relative_pointer(&pointer, &qh2, ());
                let _ = conn2.flush();
                eprintln!("[{}] lock requested ({:?}); watch for LOCKED event", ts(), locked.id());

                // t=12s: release; t=13s quit.
                let conn3 = conn2.clone();
                glib::timeout_add_local_once(Duration::from_millis(11_000), move || {
                    eprintln!("[{}] releasing lock", ts());
                    locked.destroy();
                    _rel.destroy();
                    let _ = conn3.flush();
                    glib::timeout_add_local_once(Duration::from_millis(1000), move || {
                        app2.quit();
                    });
                });
            });
        });
        app.run_with_args::<&str>(&[]);
    }
}
