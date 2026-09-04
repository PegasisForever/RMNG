//! The viewer's `NSView` subclass and its window. The view owns a `CAMetalLayer` and receives
//! mouse/keyboard events **directly** from AppKit — the raison d'être of the native viewer: no
//! GDK re-derives pointer state, so motion never stalls (the fullscreen top-edge bug), and
//! `keyCode` is the true Carbon virtual key with no `interpretKeyEvents:` mangling.
//!
//! Coordinates: AppKit delivers `locationInWindow` in points with a bottom-left origin; we invert
//! the letterbox transform (the same `contain` fit the renderer uses) to reach monitor-pixel
//! image coordinates for `pointer_move`. Ordinary motion clamps to the image; a *drag* must not,
//! because AppKit's implicit grab keeps delivering to the view the button went down in even once
//! the pointer is over the next window, and that out-of-bounds overshoot is what says which
//! neighbouring monitor the drag has crossed onto (see [`viewer_core::drag_route`]).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationPresentationOptions, NSBackingStoreType, NSCursor, NSEvent,
    NSEventModifierFlags, NSResponder, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use objc2_quartz_core::CAMetalLayer;

use viewer_core::kvk_evdev;
// Shared with the GTK viewer: the drag across the window seam is the same geometry in both
// clients, and both read modifier state out of the same flag word. Keeping one copy of each is
// what stops the two drifting apart (the modifiers already had, which stuck a modifier down).
use viewer_core::drag_route::{route_drag, Screen};
use viewer_core::kvk_modifiers::modifier_now_down;

use crate::pointer::PointerLock;
use crate::shared::{send_input, Shared, Writer};

/// The monitor layout every window routes drags against, held once for the whole window set.
///
/// It lives here rather than being copied into each `WinCtx` at construction because a `WinCtx`
/// is built only when a window's *content* is (re)created: a spec that moves a monitor without
/// touching content would otherwise leave every existing window routing against stale geometry.
/// `AppState::reconcile` refreshes the one `RefCell`, and every window sees it. Main-thread only,
/// like the rest of `WinCtx` — the GTK viewer's `SharedLayout` is the same shape.
pub type SharedLayout = Rc<RefCell<Vec<Screen>>>;

/// Per-view context: which monitor it shows, where to send input, and the state the tick needs.
/// Main-thread only, so `Rc` + `Cell`/`RefCell` rather than atomics.
pub struct WinCtx {
    pub monitor_id: u32,
    pub shared: Arc<Shared>,
    /// The desktop layout, for following a drag off this window's edge onto its neighbour.
    pub layout: SharedLayout,
    pub writer: Writer,
    /// Whether Cmd/Ctrl are swapped on the wire (Mac muscle memory → remote Ctrl).
    pub cmd_is_ctrl: bool,
    /// Pointer lock, shared across windows (a single process-wide cursor).
    pub pointer_lock: Option<Rc<PointerLock>>,
    /// Last known image size (monitor pixels); updated by `app` each draw.
    pub frame_size: Cell<(f64, f64)>,
    /// evdev keycodes currently held on the remote (released on focus loss).
    pub pressed: RefCell<HashSet<u32>>,
    /// Mouse buttons currently held (evdev), for the same reason.
    pub buttons: RefCell<HashSet<i32>>,
    /// The cursor built from the remote's latest sprite, and the shape version it came from.
    pub cursor: RefCell<Option<Retained<NSCursor>>>,
    pub cursor_version: Cell<u64>,
    /// Whether the pointer is currently over this view (drives cursor application).
    pub inside: Cell<bool>,
}

impl WinCtx {
    /// Is pointer lock currently holding the cursor?
    pub fn locked(&self) -> bool {
        self.pointer_lock.as_ref().is_some_and(|p| p.is_engaged())
    }

    /// Whether local pointer motion must be swallowed right now. Two reasons, and both apply to
    /// every path that would send an absolute `pointer_move`: pointer lock owns the cursor while
    /// engaged (the relative path sends motion, and an absolute move would yank the grab), and
    /// just after an agent-driven warp local motion is held off so the user's mouse doesn't pull
    /// the cursor off the agent's target (debounced; refreshed by each warp).
    fn motion_suppressed(&self) -> bool {
        self.locked()
            || self.shared.warp.lock().unwrap().is_some_and(|deadline| Instant::now() < deadline)
    }

    /// The image size to map against — the last frame's, falling back to 1080p before any frame
    /// has arrived, since a zero would collapse the whole transform.
    fn image_size(&self) -> (f64, f64) {
        let (fw, fh) = self.frame_size.get();
        (if fw > 0.0 { fw } else { 1920.0 }, if fh > 0.0 { fh } else { 1080.0 })
    }

    /// Map a window-space point (points, bottom-left origin) to image pixels, inverting the
    /// letterbox, **without clamping**.
    ///
    /// A drag past the window's edge arrives here (implicit grab) with a `locationInWindow`
    /// outside the view, and how far outside is precisely what says which neighbouring monitor
    /// the drag is now over — so the drag path must keep it. Everything else wants
    /// [`WinCtx::to_image`]'s clamp.
    fn to_image_unclamped(&self, view: &NSView, p: NSPoint) -> (f64, f64) {
        let bounds = view.bounds();
        let (vw, vh) = (bounds.size.width.max(1.0), bounds.size.height.max(1.0));
        // Convert to a top-left origin.
        letterbox_inverse((vw, vh), self.image_size(), (p.x, vh - p.y))
    }

    /// Map a window-space point (points, bottom-left origin) to image pixels, inverting the
    /// letterbox. Clamped to the image.
    fn to_image(&self, view: &NSView, p: NSPoint) -> (f64, f64) {
        let (fw, fh) = self.image_size();
        let (ix, iy) = self.to_image_unclamped(view, p);
        (ix.clamp(0.0, fw), iy.clamp(0.0, fh))
    }

    /// Where a drag at window point `p` really is: the monitor it has been pulled onto and the
    /// position on it. This view *is* the drag's origin — AppKit delivers every `mouseDragged:`
    /// to the view the `mouseDown:` landed in, whatever window the pointer has since moved
    /// over — so no separate origin needs tracking. `None` when this window's monitor is no
    /// longer in the layout, in which case there is nowhere honest to send the drag.
    fn drag_target(&self, view: &NSView, p: NSPoint) -> Option<(u32, f64, f64)> {
        let (mx, my) = self.to_image_unclamped(view, p);
        route_drag(&self.layout.borrow(), self.monitor_id, mx, my)
    }

    fn send_move_to(&self, monitor: u32, x: f64, y: f64) {
        send_input(
            &self.writer,
            &format!(r#"{{"kind":"pointer_move","monitor_id":{monitor},"x":{x:.1},"y":{y:.1}}}"#),
        );
    }

    fn send_move(&self, view: &NSView, ev: &NSEvent) {
        if self.motion_suppressed() {
            return;
        }
        let p = ev.locationInWindow();
        // Mid-drag (a button is held), follow the pointer across the seam: the implicit grab
        // keeps the events coming here after the pointer has left, so clamping to this window
        // would pin a remote window-drag at the monitor edge instead of letting it cross.
        let (monitor, x, y) = if self.buttons.borrow().is_empty() {
            let (x, y) = self.to_image(view, p);
            (self.monitor_id, x, y)
        } else {
            match self.drag_target(view, p) {
                Some(t) => t,
                None => return,
            }
        };
        self.send_move_to(monitor, x, y);
    }

    /// A button release ends any drag: put the remote cursor at the routed target first, so the
    /// button-up lands where the drag actually got to rather than back on the origin monitor.
    fn send_drag_end_move(&self, view: &NSView, ev: &NSEvent) {
        if self.motion_suppressed() {
            return;
        }
        if let Some((monitor, x, y)) = self.drag_target(view, ev.locationInWindow()) {
            self.send_move_to(monitor, x, y);
        }
    }

    fn send_button(&self, button: i32, pressed: bool) {
        if pressed {
            self.buttons.borrow_mut().insert(button);
        } else {
            self.buttons.borrow_mut().remove(&button);
        }
        send_input(
            &self.writer,
            &format!(r#"{{"kind":"button","button":{button},"pressed":{pressed}}}"#),
        );
    }

    /// Translate a physical kVK to the evdev code we put on the wire (applying the Cmd/Ctrl swap).
    fn evdev(&self, kvk: u32) -> u32 {
        let code = kvk_evdev::translate(kvk);
        if self.cmd_is_ctrl {
            swap_cmd_ctrl(code)
        } else {
            code
        }
    }

    /// Release every key and button this window still holds on the remote.
    pub fn release_all_input(&self) {
        for code in self.pressed.borrow_mut().drain().collect::<Vec<_>>() {
            send_input(
                &self.writer,
                &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":false}}"#),
            );
        }
        for b in self.buttons.borrow_mut().drain().collect::<Vec<_>>() {
            send_input(&self.writer, &format!(r#"{{"kind":"button","button":{b},"pressed":false}}"#));
        }
    }

    /// Apply the remote cursor shape if the pointer is over this view and not locked.
    pub fn apply_cursor(&self) {
        if self.locked() || !self.inside.get() {
            return;
        }
        if let Some(c) = self.cursor.borrow().as_ref() {
            c.set();
        }
    }
}

/// Cmd↔Ctrl swap (an involution): Cmd→Ctrl, Ctrl→Super, so Mac chords reach GNOME as Ctrl while
/// the overview stays reachable. Mirrors `crates/viewer/src/keyboard_macos.rs`.
fn swap_cmd_ctrl(evdev: u32) -> u32 {
    match evdev {
        125 => 29,  // LEFTMETA  -> LEFTCTRL
        126 => 97,  // RIGHTMETA -> RIGHTCTRL
        29 => 125,  // LEFTCTRL  -> LEFTMETA
        97 => 126,  // RIGHTCTRL -> RIGHTMETA
        other => other,
    }
}

/// Invert the renderer's `contain` letterbox: a view point in points (top-left origin) → image
/// pixels. Deliberately **unclamped**: a point outside the view maps outside the image, which is
/// how a drag past the window edge tells [`viewer_core::drag_route`] which neighbour it is over.
fn letterbox_inverse((vw, vh): (f64, f64), (fw, fh): (f64, f64), (px, py): (f64, f64)) -> (f64, f64) {
    let scale = (vw / fw).min(vh / fh);
    ((px - (vw - fw * scale) / 2.0) / scale, (py - (vh - fh * scale) / 2.0) / scale)
}

/// evdev mouse-button codes. An unknown button must NOT fall back to left (a phantom click).
fn evdev_button(ns_button: isize) -> Option<i32> {
    match ns_button {
        0 => Some(0x110), // left
        1 => Some(0x111), // right
        2 => Some(0x112), // middle
        3 => Some(0x113), // back
        4 => Some(0x114), // forward
        _ => None,
    }
}

/// Turn a wheel delta into whole scroll notches, carrying the fraction in `rem`.
///
/// macOS does not hand out unit notches: a non-precise wheel reports an *accelerated* line
/// count (3, 6, 10…), and a slow nudge can report less than one line. Truncating toward zero
/// and keeping the remainder for the next event forwards the user's real scroll distance while
/// never inventing a notch out of a sub-notch twitch. Same scheme as the GTK viewer's
/// `ScrollUnit::Wheel` path, so both clients scroll the same amount for the same gesture.
fn wheel_notches(rem: &Cell<(f64, f64)>, dx: f64, dy: f64) -> (i32, i32) {
    let (mut rx, mut ry) = rem.get();
    rx += dx;
    ry += dy;
    let (sx, sy) = (rx.trunc() as i32, ry.trunc() as i32);
    rem.set((rx - f64::from(sx), ry - f64::from(sy)));
    (sx, sy)
}

const KVK_F11: u32 = 0x67;
const KVK_G: u32 = 0x05;
const KVK_P: u32 = 0x23;

#[derive(Default)]
pub struct ViewerViewIvars {
    ctx: RefCell<Option<Rc<WinCtx>>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
    /// Sub-notch wheel delta not yet forwarded, per axis (see [`wheel_notches`]).
    wheel_rem: Cell<(f64, f64)>,
}

define_class!(
    // SAFETY:
    // - Superclass NSView imposes no subclassing requirements beyond main-thread use.
    // - This class does not implement Drop in a way that conflicts with ObjC dealloc.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewerViewIvars]
    #[name = "RmngViewerView"]
    pub struct ViewerView;

    impl ViewerView {
        #[unsafe(method(wantsLayer))]
        fn wants_layer(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: *mut NSEvent) -> bool {
            true
        }

        // Keep the tracking area covering the whole (resized / fullscreen) view so mouseMoved
        // fires everywhere, including the top edge in fullscreen.
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(old) = self.ivars().tracking.borrow_mut().take() {
                self.removeTrackingArea(&old);
            }
            let opts = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::CursorUpdate
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
                    opts,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            *self.ivars().tracking.borrow_mut() = Some(area);
        }

        // The window moved to a display with a different backing scale (or gained one at all).
        // A hosted layer's `contentsScale` does not follow the view on its own, so it is set
        // here as well as at construction — see [`make_video_view`] for why it matters.
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            // SAFETY: plain superclass call on the main thread; NSView's implementation is the
            // documented thing to chain to from an override of this method.
            unsafe { msg_send![super(self), viewDidChangeBackingProperties] }
            if let Some(layer) = self.layer() {
                layer.setContentsScale(self.backing_scale());
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            if let Some(ctx) = self.ctx() {
                ctx.inside.set(true);
                ctx.apply_cursor();
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            if let Some(ctx) = self.ctx() {
                ctx.inside.set(false);
            }
        }

        // AppKit asks who owns the cursor here; answer with the remote's shape.
        #[unsafe(method(cursorUpdate:))]
        fn cursor_update(&self, _event: &NSEvent) {
            if let Some(ctx) = self.ctx() {
                ctx.apply_cursor();
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            if let Some(ctx) = self.ctx() {
                ctx.send_move(self, event);
            }
        }
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if let Some(ctx) = self.ctx() {
                ctx.send_move(self, event);
            }
        }
        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            if let Some(ctx) = self.ctx() {
                ctx.send_move(self, event);
            }
        }
        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            if let Some(ctx) = self.ctx() {
                ctx.send_move(self, event);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.button(event, true);
        }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.button(event, false);
        }
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.button(event, true);
        }
        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.button(event, false);
        }
        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.button(event, true);
        }
        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.button(event, false);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let Some(ctx) = self.ctx() else { return };
            // Trackpads report precise deltas in points: forward them as Mutter's smooth
            // finger-source axis, which is what the GTK viewer sends for ScrollUnit::Surface.
            // A wheel reports coarse line deltas: forward those as discrete notches.
            //
            // AppKit and Mutter measure scrolling in opposite directions: `scrollingDeltaY` is
            // positive for scroll UP and `scrollingDeltaX` positive for scroll LEFT, while the
            // `axis` / `axis_continuous` messages go straight through to Mutter's Wayland /
            // libinput convention, where positive dy is DOWN and positive dx is RIGHT. Hence the
            // negation on both axes — please don't "fix" it back; forwarding the deltas raw
            // (what the GTK viewer correctly does with GDK's already-Wayland-oriented values) is
            // what made the video plane scroll backwards while the terminal plane did not.
            // macOS has already folded the user's natural-scrolling preference into the delta by
            // the time we see it, so converting the convention here honours that setting rather
            // than fighting it.
            let (dx, dy) = (-event.scrollingDeltaX(), -event.scrollingDeltaY());
            if event.hasPreciseScrollingDeltas() {
                if dx != 0.0 || dy != 0.0 {
                    send_input(
                        &ctx.writer,
                        &format!(
                            r#"{{"kind":"axis_continuous","dx":{dx},"dy":{dy},"flags":{}}}"#,
                            wire::socket::axis_flags::SOURCE_FINGER
                        ),
                    );
                }
                // Fingers lifted: finish the gesture so the remote's kinetic scroll ends.
                if event.phase() == objc2_app_kit::NSEventPhase::Ended
                    || event.momentumPhase() == objc2_app_kit::NSEventPhase::Ended
                {
                    send_input(
                        &ctx.writer,
                        &format!(
                            r#"{{"kind":"axis_continuous","dx":0.0,"dy":0.0,"flags":{}}}"#,
                            wire::socket::axis_flags::FINISH | wire::socket::axis_flags::SOURCE_FINGER
                        ),
                    );
                }
                return;
            }
            let (step_x, step_y) = wheel_notches(&self.ivars().wheel_rem, dx, dy);
            if step_y != 0 {
                send_input(&ctx.writer, &format!(r#"{{"kind":"axis","axis":0,"step":{step_y}}}"#));
            }
            if step_x != 0 {
                send_input(&ctx.writer, &format!(r#"{{"kind":"axis","axis":1,"step":{step_x}}}"#));
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let Some(ctx) = self.ctx() else { return };
            let kvk = event.keyCode() as u32;
            let mf = event.modifierFlags();
            let ctrl_alt = mf.contains(NSEventModifierFlags::Control)
                && mf.contains(NSEventModifierFlags::Option);

            // Local viewer shortcuts, never forwarded (matching the GTK viewer):
            //   F11 fullscreen · Ctrl+Alt+G toggle pointer-lock · Ctrl+Alt+P release + unstick.
            // Each drops the keys the remote still holds, because the modifiers were forwarded
            // before we knew this was a shortcut and the grab/focus change can eat their key-up.
            if kvk == KVK_F11 {
                tracing::debug!("key: F11 consumed locally (fullscreen toggle), NOT forwarded");
                if let Some(win) = self.window() {
                    win.toggleFullScreen(None);
                }
                return;
            }
            if ctrl_alt && kvk == KVK_G {
                ctx.release_all_input();
                if let Some(pl) = ctx.pointer_lock.as_ref() {
                    let want = ctx.shared.auto_lock.lock().unwrap().toggle(Instant::now());
                    if want {
                        pl.engage();
                    } else {
                        pl.release();
                    }
                }
                return;
            }
            if ctrl_alt && kvk == KVK_P {
                // Panic / unstick: force release and drop every held key + button.
                ctx.shared.auto_lock.lock().unwrap().force_release();
                if let Some(pl) = ctx.pointer_lock.as_ref() {
                    pl.release();
                }
                ctx.release_all_input();
                return;
            }

            if event.isARepeat() {
                return; // the remote autorepeats the held key itself
            }
            let code = ctx.evdev(kvk);
            if code != 0 {
                tracing::debug!("key down: kVK={kvk:#04x} evdev={code} → forwarded");
                ctx.pressed.borrow_mut().insert(code);
                send_input(
                    &ctx.writer,
                    &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":true}}"#),
                );
            }
            // Do NOT call super / interpretKeyEvents: — consume it (no beep, no text-input
            // mangling, no synthesized null-key).
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let Some(ctx) = self.ctx() else { return };
            let code = ctx.evdev(event.keyCode() as u32);
            // Only release keys we actually forwarded a press for: that skips the shortcut keys
            // and avoids phantom releases.
            if code != 0 && ctx.pressed.borrow_mut().remove(&code) {
                tracing::debug!("key up: kVK={:#04x} evdev={code} → forwarded", event.keyCode() as u32);
                send_input(
                    &ctx.writer,
                    &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":false}}"#),
                );
            }
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let Some(ctx) = self.ctx() else { return };
            let kvk = event.keyCode() as u32;
            let code = ctx.evdev(kvk);
            if code == 0 {
                return;
            }
            let mf = event.modifierFlags().0;
            // CapsLock is a lock toggle, not a hold: emit a tap so the remote flips its own lock.
            if kvk == 0x39 {
                for pressed in [true, false] {
                    send_input(
                        &ctx.writer,
                        &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":{pressed}}}"#),
                    );
                }
                return;
            }
            // Read the key's real state from THIS event's flags, never from history: a modifier
            // held across a Cmd+Tab into the viewer delivers only its release, which must read as
            // an up rather than invert into a phantom press. `None` = fn/Globe and friends, which
            // carry no remote-mappable state.
            let Some(now_down) = modifier_now_down(mf, kvk) else {
                return;
            };
            let mut held = ctx.pressed.borrow_mut();
            let forward = if now_down { held.insert(code) } else { held.remove(&code) };
            drop(held);
            if forward {
                send_input(
                    &ctx.writer,
                    &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":{now_down}}}"#),
                );
            }
        }
    }
);

impl ViewerView {
    fn ctx(&self) -> Option<Rc<WinCtx>> {
        self.ivars().ctx.borrow().clone()
    }

    fn button(&self, event: &NSEvent, pressed: bool) {
        let Some(ctx) = self.ctx() else { return };
        // The release is the last event of a drag, and it can arrive without a final
        // `mouseDragged:` — so resolve the cross-seam position here too, before the button-up
        // goes out, or a drag that ended on the neighbour would be dropped on this monitor.
        if !pressed {
            ctx.send_drag_end_move(self, event);
        }
        if let Some(b) = evdev_button(event.buttonNumber()) {
            ctx.send_button(b, pressed);
        }
    }

    /// The scale factor of the display this view is on, falling back to 1.0 before the view has
    /// a window (never 2.0: guessing Retina on a 1x display would scale the layer up by two).
    fn backing_scale(&self) -> f64 {
        self.window().map(|w| w.backingScaleFactor()).unwrap_or(1.0)
    }

    /// Is this view the first responder of its window — i.e. are typed keys actually on their
    /// way to the remote right now? What the ⌘Q / ⌘, monitor asks before stealing those chords
    /// from the menu (see `app::install_menu_chord_monitor`).
    pub fn owns_keystrokes(&self) -> bool {
        let Some(fr) = self.window().and_then(|w| w.firstResponder()) else {
            return false;
        };
        // Identity, not equality: `isEqual:` on responders is pointer equality anyway, and this
        // says plainly that the *same object* is the one AppKit would hand the key event to.
        let me: &NSResponder = self;
        std::ptr::eq(&*fr, me)
    }

    /// Release every key + button this window holds on the remote (genuine focus loss).
    pub fn release_all(&self) {
        if let Some(ctx) = self.ctx() {
            ctx.release_all_input();
        }
    }

    pub fn metal_layer(&self) -> Retained<CAMetalLayer> {
        let layer = self.layer().expect("viewer view must be layer-backed");
        layer.downcast::<CAMetalLayer>().expect("layer must be a CAMetalLayer")
    }
}

/// A bare viewer window: titled, resizable, no content yet. The shell is stable for the
/// window's whole life; only its content view swaps (video ⇄ terminal ⇄ placeholder), which is
/// what lets a clone switch without destroying and rebuilding windows.
///
/// Every window is closable, and closing any of them quits — see [`install_window_delegate`].
pub fn make_window_shell(mtm: MainThreadMarker, title: &str) -> Retained<NSWindow> {
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 720.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: standard window creation; released-when-closed is disabled below.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            content,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window.setAcceptsMouseMovedEvents(true);
    install_window_delegate(mtm, &window);
    place_cascaded(&window);
    window.makeKeyAndOrderFront(None);
    window
}

thread_local! {
    /// Where the next window's top-left goes. `cascadeTopLeftFromPoint:` answers with the point
    /// for the window *after* the one it just placed, so carrying that answer here is the whole
    /// cascade; AppKit also steps the column and wraps at the screen edge for us.
    static NEXT_CASCADE: Cell<Option<NSPoint>> = const { Cell::new(None) };
}

/// Place `window` one step down the staircase from the last one.
///
/// An N-monitor layout opens N windows at once, and centring every one of them stacks them
/// exactly on top of each other — the ones underneath can only be reached through the window
/// menu. This is deliberately just a cascade: which *physical* screen a remote monitor's window
/// belongs on is a separate question, and `AppState`'s layout is the remote geometry, not the
/// local one.
fn place_cascaded(window: &NSWindow) {
    let from = NEXT_CASCADE.with(|c| c.get());
    // The first window still lands in the middle of the screen; the staircase starts there.
    // `NSZeroPoint` means "don't move it, just tell me where the next one goes", which is
    // exactly how the cascade is seeded from a centred window.
    let start = from.unwrap_or_else(|| {
        window.center();
        NSPoint::new(0.0, 0.0)
    });
    let next = window.cascadeTopLeftFromPoint(start);
    NEXT_CASCADE.with(|c| c.set(Some(next)));
}

// ── window delegate ─────────────────────────────────────────────────────────────────────────

define_class!(
    // SAFETY: NSObject superclass has no subclassing requirements; no conflicting Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RmngWindowDelegate"]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            // Closing any window means "done with the viewer", as it does in GTK (`app.quit()`
            // there). The window set mirrors the remote desktop, so a lone window the user shut
            // would never come back — reconcile only builds windows for monitors it has none
            // for — and quitting deliberately is also what keeps a last close from leaving a
            // headless process behind, since the app must not quit on last-window-closed.
            NSApplication::sharedApplication(self.mtm()).terminate(None);
            true
        }

        // Fullscreen presentation policy — see [`hidden_menubar_options`]. One delegate is
        // shared by every window, which is right for this: the policy is about what fullscreen
        // means for a viewer window, not about which one it is, and the window AppKit asks
        // about is whichever one the user just sent to fullscreen.
        #[unsafe(method(window:willUseFullScreenPresentationOptions:))]
        fn will_use_fullscreen_presentation_options(
            &self,
            _window: &NSWindow,
            proposed: NSApplicationPresentationOptions,
        ) -> NSApplicationPresentationOptions {
            if std::env::var_os("RMNG_FULLSCREEN_MENUBAR").is_some() {
                tracing::info!(
                    "fullscreen: RMNG_FULLSCREEN_MENUBAR set — keeping the Mac menu bar's auto-hide reveal"
                );
                return proposed;
            }
            let chosen = hidden_menubar_options(proposed);
            tracing::info!(
                "fullscreen: presentation options {proposed:?} → {chosen:?} \
                 (Mac menu bar hidden, not auto-hidden; F11 leaves fullscreen)"
            );
            chosen
        }
    }
);

/// Rewrite AppKit's proposed fullscreen presentation options: swap the auto-hide menu bar /
/// Dock pair for the hidden pair and keep everything else — notably `FullScreen`, which AppKit
/// requires to stay set. `HideMenuBar` must be accompanied by `HideDock` (AppKit rejects it with
/// an auto-hidden Dock), hence both are forced together.
///
/// # Why
/// In macOS fullscreen the menu bar is only *auto-hidden* by default: parking the pointer at the
/// top edge slides the Mac menu bar and the window's titlebar down over the content. That is
/// precisely the strip a remote-desktop viewer needs to hand to the remote — the clone's GNOME
/// top bar lives there, and the operator has to be able to reach it. Asking for `HideMenuBar |
/// HideDock` instead means nothing is revealed at the top edge; F11 (consumed locally in
/// `keyDown:`) remains the way out.
///
/// Opt out (keep the stock auto-hide reveal): `RMNG_FULLSCREEN_MENUBAR=1`. Same knob, same
/// meaning, as the GTK viewer's `crates/viewer/src/fullscreen_macos.rs` — which needs 160 lines
/// of `class_addMethod` injection to reach a window class GDK owns, where we simply own ours.
fn hidden_menubar_options(
    proposed: NSApplicationPresentationOptions,
) -> NSApplicationPresentationOptions {
    (proposed
        - (NSApplicationPresentationOptions::AutoHideMenuBar
            | NSApplicationPresentationOptions::AutoHideDock))
        | NSApplicationPresentationOptions::HideMenuBar
        | NSApplicationPresentationOptions::HideDock
}

impl WindowDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // The delegate is stateless now that every window gets the same policy; `set_ivars`
        // still has to run, since super-init only accepts a partially-initialised object.
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: initialising our own NSObject subclass through its superclass's init.
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    /// The window delegate, kept alive for the whole process: AppKit holds `window.delegate`
    /// weakly, so something on our side has to own it for as long as a window points at it (the
    /// application delegate is retained by `AppState` for the same reason). It carries no
    /// per-window state, so this one instance serves every window and never grows with the set.
    static WINDOW_DELEGATE: RefCell<Option<Retained<WindowDelegate>>> =
        const { RefCell::new(None) };
}

/// Give `window` the shared delegate: closing it quits the viewer, and fullscreen hands the top
/// of the screen to the remote. Programmatic `close()` — how `reconcile` retires a window the
/// spec dropped — bypasses `windowShouldClose:`, so the quit only ever follows a user close.
pub fn install_window_delegate(mtm: MainThreadMarker, window: &NSWindow) {
    let delegate = WINDOW_DELEGATE
        .with(|d| d.borrow_mut().get_or_insert_with(|| WindowDelegate::new(mtm)).clone());
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
}

/// Build the Metal-backed video view for `window` and make it the content view.
pub fn make_video_view(
    mtm: MainThreadMarker,
    window: &NSWindow,
    ctx: Rc<WinCtx>,
    device: &ProtocolObject<dyn objc2_metal::MTLDevice>,
) -> Retained<ViewerView> {
    let frame = window.contentView().map(|v| v.bounds()).unwrap_or(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(1280.0, 720.0),
    ));
    let view = {
        let this = ViewerView::alloc(mtm).set_ivars(ViewerViewIvars {
            ctx: RefCell::new(Some(ctx)),
            tracking: RefCell::new(None),
            wheel_rem: Cell::new((0.0, 0.0)),
        });
        let this: Retained<ViewerView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this
    };
    view.setWantsLayer(true);
    // Install a CAMetalLayer as the view's backing layer.
    let layer = CAMetalLayer::layer();
    layer.setDevice(Some(device));
    layer.setPixelFormat(objc2_metal::MTLPixelFormat::BGRA8Unorm);
    layer.setFramebufferOnly(true);
    // A layer we host ourselves does not inherit the view's scale — `setWantsLayer:` only does
    // that for the layer AppKit makes. The draw path sizes the drawable from
    // `backingScaleFactor` regardless, so the picture already lands at native resolution; the
    // scale still has to be right or everything else the layer measures in points (its own
    // geometry, and any content Core Animation rasterises) is off by 2× on a Retina display.
    // (Asked of the window, not the view: the view is not in it yet, so it has no display of
    // its own to answer for. `viewDidChangeBackingProperties` keeps this current afterwards.)
    layer.setContentsScale(window.backingScaleFactor());
    view.setLayer(Some(&layer));

    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A swap, not a one-way map: physical Control must still produce Super, or the GNOME
    /// overview becomes unreachable from a Mac keyboard.
    #[test]
    fn cmd_ctrl_swap_goes_both_ways_and_is_an_involution() {
        assert_eq!(swap_cmd_ctrl(125), 29);
        assert_eq!(swap_cmd_ctrl(29), 125);
        assert_eq!(swap_cmd_ctrl(126), 97);
        assert_eq!(swap_cmd_ctrl(97), 126);
        for code in [125u32, 126, 29, 97, 30, 58, 0] {
            assert_eq!(swap_cmd_ctrl(swap_cmd_ctrl(code)), code, "code {code}");
        }
    }

    /// An unrecognised button must map to nothing rather than falling back to left, which would
    /// inject phantom clicks on the remote.
    #[test]
    fn unknown_mouse_buttons_map_to_nothing() {
        assert_eq!(evdev_button(0), Some(0x110));
        assert_eq!(evdev_button(1), Some(0x111));
        assert_eq!(evdev_button(2), Some(0x112));
        assert_eq!(evdev_button(7), None);
    }

    /// `viewer-core` spells the modifier class bits as literals so it can stay free of any
    /// platform framework; if AppKit's values ever disagreed with them, every modifier would
    /// silently break. Fail here instead.
    #[test]
    fn core_class_flags_match_appkit() {
        use viewer_core::kvk_modifiers::{CLASS_COMMAND, CLASS_CONTROL, CLASS_OPTION, CLASS_SHIFT};
        assert_eq!(CLASS_SHIFT, NSEventModifierFlags::Shift.0);
        assert_eq!(CLASS_CONTROL, NSEventModifierFlags::Control.0);
        assert_eq!(CLASS_OPTION, NSEventModifierFlags::Option.0);
        assert_eq!(CLASS_COMMAND, NSEventModifierFlags::Command.0);
    }

    /// The overshoot is the whole signal for cross-monitor drag routing, so the inverse must
    /// carry a point outside the view straight out of the image rather than folding it back in.
    /// (`to_image` clamps on top of this for ordinary motion; this is what the drag path uses.)
    #[test]
    fn the_letterbox_inverse_keeps_points_outside_the_view() {
        // Matched aspect: the transform is a pure scale, no bars.
        let (v, f) = ((960.0, 540.0), (1920.0, 1080.0));
        assert_eq!(letterbox_inverse(v, f, (0.0, 0.0)), (0.0, 0.0));
        assert_eq!(letterbox_inverse(v, f, (480.0, 270.0)), (960.0, 540.0));
        // One point past the right edge is two pixels past the image — the drag has crossed.
        assert_eq!(letterbox_inverse(v, f, (961.0, 270.0)), (1922.0, 540.0));
        // And off the left/top, negative, which routes onto the monitor on that side.
        assert_eq!(letterbox_inverse(v, f, (-10.0, -5.0)), (-20.0, -10.0));

        // Pillarboxed (a 16:9 image in a 2:1 window): the bars are not part of the image, so a
        // point inside the left bar is already a negative image coordinate.
        let (v, f) = ((2000.0, 1000.0), (1920.0, 1080.0));
        let scale = 1000.0 / 1080.0;
        let bar = (2000.0 - 1920.0 * scale) / 2.0;
        assert_eq!(letterbox_inverse(v, f, (bar, 0.0)), (0.0, 0.0));
        let (ix, _) = letterbox_inverse(v, f, (bar - 1.0, 0.0));
        assert!(ix < 0.0, "a point in the left bar is left of the image, got {ix}");
        let (ix, _) = letterbox_inverse(v, f, (2000.0, 500.0));
        assert!(ix > 1920.0, "a point past the right bar is right of the image, got {ix}");
    }

    /// The fullscreen policy the operator actually needs: the top strip of the screen must stay
    /// the remote's, so the Mac menu bar has to be *hidden*, not auto-hidden (which reveals it —
    /// and the titlebar — the moment the pointer reaches the top edge). Mirrors the GTK viewer's
    /// tests in `crates/viewer/src/fullscreen_macos.rs`, which is the same policy.
    #[test]
    fn fullscreen_swaps_the_auto_hide_pair_for_the_hidden_pair() {
        use NSApplicationPresentationOptions as O;
        let stock = O::FullScreen | O::AutoHideMenuBar | O::AutoHideDock;
        let got = hidden_menubar_options(stock);
        assert!(got.contains(O::HideMenuBar | O::HideDock));
        assert!(!got.intersects(O::AutoHideMenuBar | O::AutoHideDock));
        // AppKit requires FullScreen to survive the delegate's answer, and unrelated bits are
        // none of our business.
        assert!(got.contains(O::FullScreen));
        assert!(hidden_menubar_options(stock | O::AutoHideToolbar).contains(O::AutoHideToolbar));
    }

    /// `HideMenuBar` alongside an auto-hidden Dock is an invalid AppKit combination, so the Dock
    /// bit has to be forced even when only the menu bar's was proposed.
    #[test]
    fn a_hidden_menu_bar_always_brings_a_hidden_dock() {
        use NSApplicationPresentationOptions as O;
        let got = hidden_menubar_options(O::FullScreen | O::AutoHideMenuBar);
        assert!(got.contains(O::HideDock));
        assert!(!got.contains(O::AutoHideDock));
    }

    /// A wheel notch arrives as an accelerated line count, so the magnitude has to survive:
    /// truncate to whole notches and carry the fraction. Collapsing to ±1 (what this replaced)
    /// made a fast spin scroll several times slower than the GTK client, while a sub-notch
    /// delta still moved a whole line.
    #[test]
    fn wheel_notches_keep_magnitude_and_carry_the_remainder() {
        let rem = Cell::new((0.0, 0.0));
        // An accelerated spin forwards every line, not one.
        assert_eq!(wheel_notches(&rem, 0.0, 6.0), (0, 6));
        // Sub-notch deltas accumulate rather than each rounding up to a full notch.
        assert_eq!(wheel_notches(&rem, 0.0, 0.5), (0, 0));
        assert_eq!(wheel_notches(&rem, 0.0, 0.5), (0, 1));
        // The two axes carry their remainders independently.
        let rem = Cell::new((0.0, 0.0));
        assert_eq!(wheel_notches(&rem, 0.5, 2.5), (0, 2));
        assert_eq!(wheel_notches(&rem, 0.5, 0.5), (1, 1));
        // Truncation is toward zero, so scrolling back loses nothing either.
        let rem = Cell::new((0.0, 0.0));
        assert_eq!(wheel_notches(&rem, 0.0, -0.5), (0, 0));
        assert_eq!(wheel_notches(&rem, 0.0, -0.5), (0, -1));
    }
}
