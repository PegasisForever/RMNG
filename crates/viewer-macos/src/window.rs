//! The viewer's `NSView` subclass and its window. The view owns a `CAMetalLayer` and receives
//! mouse/keyboard events **directly** from AppKit — the raison d'être of the native viewer: no
//! GDK re-derives pointer state, so motion never stalls (the fullscreen top-edge bug), and
//! `keyCode` is the true Carbon virtual key with no `interpretKeyEvents:` mangling.
//!
//! Coordinates: AppKit delivers `locationInWindow` in points with a bottom-left origin; we invert
//! the letterbox transform (the same `contain` fit the renderer uses) to reach monitor-pixel
//! image coordinates for `pointer_move`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSEvent, NSEventModifierFlags, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use objc2_quartz_core::CAMetalLayer;

use viewer_core::kvk_evdev;

use crate::shared::{send_input, Shared, Writer};

/// Per-view context: which monitor it shows, where to send input, and the last frame geometry
/// (for the letterbox inverse). Held by the AppKit view via its ivars and by `app` for drawing.
pub struct WinCtx {
    pub monitor_id: u32,
    pub shared: Arc<Shared>,
    pub writer: Writer,
    /// Whether Cmd/Ctrl are swapped on the wire (Mac muscle memory → remote Ctrl).
    pub cmd_is_ctrl: bool,
    /// Last known image size (monitor pixels) for this window; updated by `app` each draw.
    pub frame_size: Cell<(f64, f64)>,
    /// evdev keycodes currently held on the remote (released on focus loss).
    pub pressed: RefCell<std::collections::HashSet<u32>>,
    /// Mouse buttons currently held (evdev), for the same reason.
    pub buttons: RefCell<std::collections::HashSet<i32>>,
}

impl WinCtx {
    /// Map a window-space point (points, bottom-left origin) to `(monitor_id, x, y)` in image
    /// pixels, inverting the letterbox. Clamped to the image.
    fn to_image(&self, view: &NSView, p: NSPoint) -> (f64, f64) {
        let bounds = view.bounds();
        let (vw, vh) = (bounds.size.width.max(1.0), bounds.size.height.max(1.0));
        // Convert to top-left origin.
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
        // Suppress local motion briefly after an agent warp (debounced), like the GTK viewer.
        if self
            .shared
            .warp
            .lock()
            .unwrap()
            .is_some_and(|deadline| std::time::Instant::now() < deadline)
        {
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

/// evdev mouse-button codes.
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
        // Layer-backed, Metal.
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

        // Keep the tracking area covering the whole (possibly resized/fullscreen) view so
        // mouseMoved fires everywhere, including the top edge in fullscreen.
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(old) = self.ivars().tracking.borrow_mut().take() {
                self.removeTrackingArea(&old);
            }
            let opts = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
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
            let dy = event.scrollingDeltaY();
            let dx = event.scrollingDeltaX();
            // Discrete wheel notches (line scroll). Precise (trackpad) deltas are coalesced into
            // notches too for now; smooth axis_continuous is a later refinement.
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
            // Local shortcut: F11 toggles fullscreen, not forwarded.
            if kvk == KVK_F11 {
                tracing::debug!("key: F11 consumed locally (fullscreen toggle), NOT forwarded");
                if let Some(win) = self.window() {
                    win.toggleFullScreen(None);
                }
                return;
            }
            if event.isARepeat() {
                return; // the remote autorepeats the held key itself
            }
            let code = ctx.evdev(kvk);
            if code != 0 {
                tracing::debug!("key down: kVK={kvk:#04x} evdev={code} → forwarded");
                ctx.pressed.borrow_mut().insert(code);
                send_input(&ctx.writer, &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":true}}"#));
            }
            // Do NOT call super/interpretKeyEvents: — consume it (no beep, no text-input mangling).
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let Some(ctx) = self.ctx() else { return };
            let code = ctx.evdev(event.keyCode() as u32);
            if code != 0 && ctx.pressed.borrow_mut().remove(&code) {
                tracing::debug!("key up: kVK={:#04x} evdev={code} → forwarded", event.keyCode() as u32);
                send_input(&ctx.writer, &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":false}}"#));
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
            // Read the modifier's real state from the event's class flag, then forward a
            // transition only when it changes the remote's view (mirrors keyboard_macos.rs).
            let mf = event.modifierFlags().0;
            let class = match kvk {
                0x3B | 0x3E => NSEventModifierFlags::Control.0,
                0x38 | 0x3C => NSEventModifierFlags::Shift.0,
                0x37 | 0x36 => NSEventModifierFlags::Command.0,
                0x3A | 0x3D => NSEventModifierFlags::Option.0,
                0x39 => {
                    // CapsLock: a toggle, not a hold — emit a tap.
                    send_input(&ctx.writer, &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":true}}"#));
                    send_input(&ctx.writer, &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":false}}"#));
                    return;
                }
                _ => return, // fn/Globe etc: no remote-mappable state
            };
            let now_down = mf & class != 0;
            let mut held = ctx.pressed.borrow_mut();
            let forward = if now_down { held.insert(code) } else { held.remove(&code) };
            if forward {
                send_input(&ctx.writer, &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":{now_down}}}"#));
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
        if let Some(b) = evdev_button(event.buttonNumber()) {
            ctx.send_button(b, pressed);
        }
    }

    /// Release every key + button this window still holds on the remote (genuine focus loss).
    pub fn release_all(&self) {
        let Some(ctx) = self.ctx() else { return };
        for code in ctx.pressed.borrow_mut().drain().collect::<Vec<_>>() {
            send_input(&ctx.writer, &format!(r#"{{"kind":"key_code","keycode":{code},"pressed":false}}"#));
        }
        for b in ctx.buttons.borrow_mut().drain().collect::<Vec<_>>() {
            send_input(&ctx.writer, &format!(r#"{{"kind":"button","button":{b},"pressed":false}}"#));
        }
    }

    pub fn metal_layer(&self) -> Retained<CAMetalLayer> {
        // The view is layer-backed with a CAMetalLayer (installed in `make_window`).
        let layer = self.layer().expect("viewer view must be layer-backed");
        layer.downcast::<CAMetalLayer>().expect("layer must be a CAMetalLayer")
    }
}

/// A viewer window for one monitor: a titled NSWindow whose content view is our Metal-backed
/// `ViewerView`. Returns the window and the view (the caller keeps the view for drawing).
pub fn make_window(
    mtm: MainThreadMarker,
    ctx: Rc<WinCtx>,
    device: &ProtocolObject<dyn objc2_metal::MTLDevice>,
    title: &str,
) -> (Retained<NSWindow>, Retained<ViewerView>) {
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 720.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: standard window creation; released-when-closed disabled below.
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
    window.setTitle(&objc2_foundation::NSString::from_str(title));
    window.setAcceptsMouseMovedEvents(true);

    let view = {
        let this = ViewerView::alloc(mtm).set_ivars(ViewerViewIvars {
            ctx: RefCell::new(Some(ctx)),
            tracking: RefCell::new(None),
        });
        let this: Retained<ViewerView> = unsafe { msg_send![super(this), initWithFrame: content] };
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
    window.center();
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&view));
    (window, view)
}
