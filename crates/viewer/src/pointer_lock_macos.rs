//! macOS pointer lock: `CGAssociateMouseAndMouseCursorPosition` + `NSEvent` local monitor.
//!
//! Implements the same public surface as `pointer_lock.rs` (Wayland) using:
//! - `CGDisplay::associate_mouse_and_mouse_cursor_position(false)` to freeze the cursor in place.
//! - An `NSEvent` local monitor (`addLocalMonitorForEventsMatchingMask:handler:`) for relative
//!   deltas — in-process, main-thread, **no TCC permission required**.
//! - `NSCursor::hide()` / `NSCursor::unhide()` for cursor visibility.
//!
//! Delta delivery mirrors the Wayland module exactly:
//! - Wire framing: `[0u8][u32be len][JSON]`
//! - JSON: `{"kind":"pointer_relative","dx":…,"dy":…}`
//! - Sub-pixel accumulation: integer-unit deltas sent; fractional remainder carried forward,
//!   exactly mirroring `pointer_lock.rs`'s `rem_x`/`rem_y` logic.
//!
//! **Thread safety**: `NSEvent` monitors must be installed from the main thread.
//! `engage` and `release` are always called from GTK signal handlers, which run on the
//! main thread, satisfying this requirement. No cross-thread machinery is needed.
//!
//! Note on delta acceleration: `NSEvent.deltaX/Y` are OS-accelerated deltas (pointer
//! acceleration applied). The remote (Mutter + the game) applies its own acceleration,
//! so this stacks a second curve. Follow-up: switch to `GCMouse.mouseMovedHandler`
//! (GameController framework, macOS 14+) for raw, unaccelerated deltas — SDL3 does this.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::net::TcpStream;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use core_graphics::display::CGDisplay;
use gtk4::gdk;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSCursor, NSEvent, NSEventMask};

/// The viewer's input write half (port-1 socket); shared with the GTK thread.
type Writer = Arc<Mutex<Option<TcpStream>>>;

/// Frame one input message to the server: `[0u8][u32be len][json]` (tag 0 = input).
/// Mirrors `pointer_lock.rs` (Wayland) byte-for-byte — same framing, same JSON shape.
fn send_relative(writer: &Writer, dx: f64, dy: f64) {
    let json = format!(r#"{{"kind":"pointer_relative","dx":{dx},"dy":{dy}}}"#);
    if let Some(g) = writer.lock().unwrap().as_mut() {
        let hdr = (json.len() as u32).to_be_bytes();
        let _ = g
            .write_all(&[0u8])
            .and_then(|_| g.write_all(&hdr))
            .and_then(|_| g.write_all(json.as_bytes()));
    }
}

pub struct PointerLock {
    writer: Writer,
    /// Opaque monitor token returned by `addLocalMonitorForEventsMatchingMask:handler:`.
    /// `None` when not engaged; stored so `release` can call `removeMonitor:`.
    monitor: RefCell<Option<Retained<AnyObject>>>,
    engaged: Cell<bool>,
}

impl PointerLock {
    /// Construct the macOS pointer-lock helper.
    ///
    /// Returns `None` when `RMNG_NO_POINTER_LOCK=1` is set.
    /// Unlike the Wayland twin, construction always succeeds on macOS (no protocol
    /// negotiation: `CGAssociateMouseAndMouseCursorPosition` is always available).
    pub fn new(_display: &gdk::Display, writer: Writer) -> Option<Self> {
        if std::env::var_os("RMNG_NO_POINTER_LOCK").is_some() {
            return None;
        }
        tracing::info!("macOS pointer lock ready (Ctrl+Alt+G toggles, Ctrl+Alt+P releases)");
        Some(PointerLock {
            writer,
            monitor: RefCell::new(None),
            engaged: Cell::new(false),
        })
    }

    pub fn is_engaged(&self) -> bool {
        self.engaged.get()
    }

    /// Freeze the cursor and start capturing relative mouse deltas.
    ///
    /// # Panics (debug)
    /// Call sites are GTK signal handlers — always the main thread.
    pub fn engage(&self, _surface: &gdk::Surface) {
        if self.engaged.get() {
            return;
        }

        // Disassociate the cursor from physical pointer position.
        // The cursor stays frozen at its current location; no per-frame warping needed —
        // the OS holds the freeze until CGAssociateMouseAndMouseCursorPosition(true).
        let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(false);
        // Hide the local cursor so the remote's cursor texture (set via GDK) is visible.
        NSCursor::hide();

        // Sub-pixel accumulator shared across block invocations.
        // Mirrors pointer_lock.rs (Wayland) State::rem_x / rem_y.
        let acc: Arc<Mutex<(f64, f64)>> = Arc::new(Mutex::new((0.0, 0.0)));

        let writer = self.writer.clone();

        // Local NSEvent monitor: runs in-process, on the main thread, no TCC permission.
        // The block receives each mouse-moved / drag event and extracts deltaX/Y.
        let block = {
            let (writer, acc) = (writer.clone(), acc.clone());
            RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
                // SAFETY: the ObjC runtime passes a valid, non-null NSEvent pointer.
                let (dx, dy) = unsafe { (event.as_ref().deltaX(), event.as_ref().deltaY()) };
                // Integer-unit batching with sub-pixel carry-forward.
                // Mirrors Dispatch<ZwpRelativePointerV1, ()>::event in pointer_lock.rs:
                //   state.rem_x += dx_unaccel; … trunc → send; subtract trunc.
                let mut g = acc.lock().unwrap();
                g.0 += dx;
                g.1 += dy;
                let ix = g.0.trunc();
                let iy = g.1.trunc();
                g.0 -= ix;
                g.1 -= iy;
                drop(g);
                if ix != 0.0 || iy != 0.0 {
                    send_relative(&writer, ix, iy);
                }
                // Return the event so GTK / AppKit continue to receive it.
                // (Returning null would suppress the event; we don't want that.)
                event.as_ptr()
            })
        };

        let mask = NSEventMask::MouseMoved
            | NSEventMask::LeftMouseDragged
            | NSEventMask::RightMouseDragged
            | NSEventMask::OtherMouseDragged;

        // SAFETY: called on the main thread; block is heap-allocated via RcBlock.
        let monitor =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        *self.monitor.borrow_mut() = monitor;
        self.engaged.set(true);
        tracing::info!("macOS pointer lock engaged");
    }

    /// Re-associate the cursor and remove the event monitor.
    ///
    /// Idempotent when not engaged.
    pub fn release(&self) {
        if !self.engaged.get() {
            return;
        }
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            // SAFETY: monitor is the value returned by addLocalMonitor…; correct type.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        NSCursor::unhide();
        let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
        self.engaged.set(false);
        tracing::info!("macOS pointer lock released");
    }
}
