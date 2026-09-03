//! Pointer lock (games): freeze the cursor and stream unaccelerated-ish relative deltas.
//!
//! `CGAssociateMouseAndMouseCursorPosition(false)` pins the cursor where it is; an `NSEvent`
//! local monitor supplies `deltaX/deltaY` for each motion. Both are in-process and need **no
//! TCC permission** (unlike a `CGEventTap`), which is why the viewer uses this pair rather than
//! capturing at the HID level. Ported from the GTK viewer's `pointer_lock_macos.rs`, minus the
//! `gdk::Surface` argument — here there is no compositor surface to attach to.
//!
//! Delivery matches the Wayland twin byte for byte: `{"kind":"pointer_relative","dx":…,"dy":…}`
//! with integer-unit sends and a fractional carry, so slow drags don't quantise away.
//!
//! `NSEvent.deltaX/Y` are OS-accelerated, so the remote's own acceleration stacks a second
//! curve — the same caveat the GTK viewer carries. `RMNG_NO_POINTER_LOCK=1` disables the whole
//! thing.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use core_graphics::display::CGDisplay;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSCursor, NSEvent, NSEventMask};

use crate::shared::{send_input, Writer};

pub struct PointerLock {
    writer: Writer,
    /// The opaque monitor token from `addLocalMonitorForEventsMatchingMask:handler:`; `None`
    /// while released, kept so `release` can remove it.
    monitor: RefCell<Option<Retained<AnyObject>>>,
    engaged: Cell<bool>,
}

impl PointerLock {
    /// `None` when `RMNG_NO_POINTER_LOCK` is set. Construction otherwise always succeeds — unlike
    /// Wayland there is no protocol to negotiate.
    pub fn new(writer: Writer) -> Option<Self> {
        if std::env::var_os("RMNG_NO_POINTER_LOCK").is_some() {
            return None;
        }
        tracing::info!("macOS pointer lock ready (Ctrl+Alt+G toggles, Ctrl+Alt+P releases)");
        Some(PointerLock { writer, monitor: RefCell::new(None), engaged: Cell::new(false) })
    }

    pub fn is_engaged(&self) -> bool {
        self.engaged.get()
    }

    /// Freeze the cursor and start streaming relative deltas. Main thread only (the NSEvent
    /// monitor requires it); idempotent.
    pub fn engage(&self) {
        if self.engaged.get() {
            return;
        }
        // Sub-pixel accumulator shared across block invocations (mirrors the Wayland rem_x/rem_y).
        let acc: Arc<Mutex<(f64, f64)>> = Arc::new(Mutex::new((0.0, 0.0)));
        let writer = self.writer.clone();
        let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: the runtime passes a valid, non-null NSEvent for the masked types.
            let (dx, dy) = unsafe { (event.as_ref().deltaX(), event.as_ref().deltaY()) };
            let mut g = acc.lock().unwrap();
            g.0 += dx;
            g.1 += dy;
            let (ix, iy) = (g.0.trunc(), g.1.trunc());
            g.0 -= ix;
            g.1 -= iy;
            drop(g);
            if ix != 0.0 || iy != 0.0 {
                send_input(&writer, &format!(r#"{{"kind":"pointer_relative","dx":{ix},"dy":{iy}}}"#));
            }
            // Return the event so AppKit keeps delivering it (null would swallow it).
            event.as_ptr()
        });
        let mask = NSEventMask::MouseMoved
            | NSEventMask::LeftMouseDragged
            | NSEventMask::RightMouseDragged
            | NSEventMask::OtherMouseDragged;
        // SAFETY: main thread; the block is heap-allocated by RcBlock.
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        // Install the monitor FIRST: if the runtime declines we bail with nothing to roll back.
        let Some(monitor) = monitor else {
            tracing::warn!(
                "macOS pointer lock: NSEvent local monitor install failed; pointer lock NOT engaged"
            );
            return;
        };
        NSCursor::hide();
        // A failed disassociation means the cursor is NOT frozen, so relative deltas would arrive
        // while absolute motion still moved the pointer. Abort and undo the hide.
        if let Err(e) = CGDisplay::associate_mouse_and_mouse_cursor_position(false) {
            tracing::warn!(
                "macOS pointer lock: CGAssociateMouseAndMouseCursorPosition(false) failed \
                 (error {e}); pointer lock NOT engaged"
            );
            unsafe { NSEvent::removeMonitor(&monitor) };
            NSCursor::unhide();
            return;
        }
        *self.monitor.borrow_mut() = Some(monitor);
        self.engaged.set(true);
        tracing::info!("macOS pointer lock engaged");
    }

    /// Re-associate the cursor and remove the monitor. Idempotent.
    pub fn release(&self) {
        if !self.engaged.get() {
            return;
        }
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            // SAFETY: the token came from addLocalMonitor…; correct type.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        NSCursor::unhide();
        if let Err(e) = CGDisplay::associate_mouse_and_mouse_cursor_position(true) {
            tracing::warn!(
                "macOS pointer lock: re-associating the cursor failed (error {e}); the cursor may \
                 stay frozen"
            );
        }
        self.engaged.set(false);
        tracing::info!("macOS pointer lock released");
    }
}
