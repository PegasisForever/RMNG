//! The viewer's `NSView` subclass and its window. The view owns a `CAMetalLayer` and receives
//! mouse/keyboard events **directly** from AppKit — the raison d'être of the native viewer: no
//! GDK re-derives pointer state, so motion never stalls (the fullscreen top-edge bug), and
//! `keyCode` is the true Carbon virtual key with no `interpretKeyEvents:` mangling.
//!
//! Coordinates: AppKit delivers `locationInWindow` in points with a bottom-left origin; we invert
//! the letterbox transform (the same `contain` fit the renderer uses) to reach monitor-pixel
//! image coordinates for `pointer_move`.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSCursor, NSEvent, NSEventModifierFlags, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CAMetalLayer;

use viewer_core::kvk_evdev;

use crate::pointer::PointerLock;
use crate::shared::{send_input, Shared, Writer};

/// Per-view context: which monitor it shows, where to send input, and the state the tick needs.
/// Main-thread only, so `Rc` + `Cell`/`RefCell` rather than atomics.
pub struct WinCtx {
    pub monitor_id: u32,
    pub shared: Arc<Shared>,
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
    fn locked(&self) -> bool {
        self.pointer_lock.as_ref().is_some_and(|p| p.is_engaged())
    }

    /// Map a window-space point (points, bottom-left origin) to image pixels, inverting the
    /// letterbox. Clamped to the image.
    fn to_image(&self, view: &NSView, p: NSPoint) -> (f64, f64) {
        let bounds = view.bounds();
        let (vw, vh) = (bounds.size.width.max(1.0), bounds.size.height.max(1.0));
        // Convert to a top-left origin.
        let (px, py) = (p.x, vh - p.y);
        let (fw, fh) = self.frame_size.get();
        let (fw, fh) = (if fw > 0.0 { fw } else { 1920.0 }, if fh > 0.0 { fh } else { 1080.0 });
        let scale = (vw / fw).min(vh / fh);
        let off_x = (vw - fw * scale) / 2.0;
        let off_y = (vh - fh * scale) / 2.0;
        let ix = ((px - off_x) / scale).clamp(0.0, fw);
        let iy = ((py - off_y) / scale).clamp(0.0, fh);
        (ix, iy)
    }

    fn send_move(&self, view: &NSView, ev: &NSEvent) {
        // Pointer lock owns motion while engaged: the relative path sends it, and an absolute
        // move here would yank the grabbed pointer.
        if self.locked() {
            return;
        }
        // Just after an agent-driven warp, hold off local motion so the user's mouse doesn't pull
        // the cursor off the agent's target (debounced; refreshed by each warp).
        if self.shared.warp.lock().unwrap().is_some_and(|deadline| Instant::now() < deadline) {
            return;
        }
        let (x, y) = self.to_image(view, ev.locationInWindow());
        send_input(
            &self.writer,
            &format!(
                r#"{{"kind":"pointer_move","monitor_id":{},"x":{x:.1},"y":{y:.1}}}"#,
                self.monitor_id
            ),
        );
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

const KVK_F11: u32 = 0x67;
const KVK_G: u32 = 0x05;
const KVK_P: u32 = 0x23;

#[derive(Default)]
pub struct ViewerViewIvars {
    ctx: RefCell<Option<Rc<WinCtx>>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
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
            let (dx, dy) = (event.scrollingDeltaX(), event.scrollingDeltaY());
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
            let step_y = if dy > 0.0 { 1 } else if dy < 0.0 { -1 } else { 0 };
            let step_x = if dx > 0.0 { 1 } else if dx < 0.0 { -1 } else { 0 };
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
            let Some(class) = modifier_class_flag(kvk) else {
                return; // fn/Globe and friends carry no remote-mappable state
            };
            // Read the key's real state from THIS event's flags, never from history: a modifier
            // held across a Cmd+Tab into the viewer delivers only its release, which must read as
            // an up rather than invert into a phantom press.
            let now_down = mf & class != 0;
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

/// The device-independent `NSEventModifierFlags` class bit for a modifier kVK. These high bits
/// are always present, so they are the reliable "is a key of this class down" signal; `keyCode`
/// already says which of the pair it is.
fn modifier_class_flag(kvk: u32) -> Option<usize> {
    Some(match kvk {
        0x3B | 0x3E => NSEventModifierFlags::Control.0, // Control / RightControl
        0x38 | 0x3C => NSEventModifierFlags::Shift.0,   // Shift / RightShift
        0x37 | 0x36 => NSEventModifierFlags::Command.0, // Command / RightCommand
        0x3A | 0x3D => NSEventModifierFlags::Option.0,  // Option / RightOption
        _ => return None,
    })
}

impl ViewerView {
    fn ctx(&self) -> Option<Rc<WinCtx>> {
        self.ivars().ctx.borrow().clone()
    }

    fn button(&self, event: &NSEvent, pressed: bool) {
        let Some(ctx) = self.ctx() else { return };
        if let Some(b) = evdev_button(event.buttonNumber()) {
            ctx.send_button(b, pressed);
        }
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
    window.center();
    window.makeKeyAndOrderFront(None);
    window
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

    #[test]
    fn only_real_modifier_kvks_have_a_class_flag() {
        assert!(modifier_class_flag(0x3B).is_some(), "Control");
        assert!(modifier_class_flag(0x37).is_some(), "Command");
        assert!(modifier_class_flag(0x3F).is_none(), "fn/Globe");
        assert!(modifier_class_flag(0x00).is_none(), "letter A");
    }
}
