//! macOS native titlebar: swap the GTK `HeaderBar` for a real `NSWindow` titlebar with a
//! native `NSButton` accessory (server-address settings). We intentionally do NOT add a
//! custom fullscreen/maximize button — the native green traffic-light button already
//! provides maximize/fullscreen on macOS.
//!
//! # Design
//! This module is macOS-only (`#[cfg(target_os = "macos")]` on the `mod` declaration in
//! `main.rs`). On Linux the GTK `HeaderBar` path remains byte-identical; nothing here is
//! compiled on Linux.
//!
//! The key steps are:
//! 1. Wait for the `GtkWindow` surface to be realized (the `NSWindow` only exists after that).
//! 2. Reach the `NSWindow` via a raw FFI call to `gdk_macos_surface_get_native_window`, which
//!    lives in `libgtk-4.dylib` (exported, even though the Rust `gdk4-macos` crate is not a
//!    direct dependency here). We call it by declaring an `extern "C"` link stub.
//! 3. Cast the resulting `gpointer` to `*mut objc2_app_kit::NSWindow` and wrap it with
//!    `unsafe { &*ptr }` — same live ObjC object, raw-pointer interop is valid.
//! 4. Configure the titlebar: ensure `Titled` style mask, set title, `titlebarAppearsTransparent
//!    = false`, `TitleVisibility = Visible`.
//! 5. Build an `NSTitlebarAccessoryViewController` whose `view` holds the settings `NSButton`
//!    (primary window only).
//! 6. Wire each button to a Rust closure via a tiny `NSObject` subclass (`ActionTarget`) that
//!    stores a `Box<dyn Fn()>` ivar. The action method reads the ivar and calls the closure.
//!    AppKit fires button actions on the main thread — the same thread as GTK's main loop on
//!    macOS — so calling GTK from those closures is safe.
//!
//! # Spike unknowns
//! The crux unknown is whether GTK4-macos will cooperate: if GTK draws its own CSD titlebar bar
//! in addition to the native one we configure here, the user will see a double titlebar. The
//! diagnostic `tracing::info!` lines below are how we distinguish that at runtime.
//!
//! # Safety policy
//! Every `unsafe` block carries a `// SAFETY:` comment. `unsafe` use is minimal.

use std::ffi::c_void;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use gtk4::prelude::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSImage, NSLayoutAttribute, NSTitlebarAccessoryViewController, NSView, NSWindow,
    NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSRect, NSSize, NSString, ns_string};

/// Input/clipboard write half (None while disconnected) — matches `main.rs`'s `Writer` type.
type Writer = Arc<Mutex<Option<TcpStream>>>;
/// The server `host:port` — matches `main.rs`'s `ServerAddr` type.
type ServerAddr = Arc<Mutex<String>>;

// ─── FFI bridge to gdk-macos ──────────────────────────────────────────────────────────────────
//
// `gdk_macos_surface_get_native_window` is exported by `libgtk-4.dylib` (GTK 4.8+). We declare
// it here rather than pulling in the `gdk4-macos` crate, avoiding a version-constraint puzzle.
// The function takes a `GdkMacosSurface *` (a GObject) and returns the raw `NSWindow *` as a
// `gpointer` (i.e. `*mut c_void`).  We already have the raw GDK surface pointer from
// `NativeExt::surface()` + `glib::translate::ToGlibPtr::to_glib_none().0`.
// SAFETY: the C function is exported by libgtk-4.dylib (GTK 4.8+, verified on this 4.22.4 build).
// We are responsible for passing a valid GdkMacosSurface* and interpreting the result as NSWindow*.
unsafe extern "C" {
    /// Returns the live `NSWindow *` for a realized `GdkMacosSurface`.
    /// Available since GTK 4.8; this Mac has GTK 4.22.4.
    fn gdk_macos_surface_get_native_window(surface: *mut c_void) -> *mut c_void;
}

// ─── Action-target bridge ─────────────────────────────────────────────────────────────────────
//
// AppKit button actions are ObjC messages: `-(void)buttonClicked:(id)sender`.
// We define a tiny NSObject subclass that stores one `Box<dyn Fn()>` closure as an ivar.
// The action method reads the ivar and calls the closure.
//
// Lifetime: the `Retained<ActionTarget>` is moved into the GTK `realize` closure via
// `std::mem::forget`, so it lives for the rest of the process. This is deliberate and is
// acceptable for a spike (and even for production, as the window lives as long as the app).

struct ActionTargetIvars {
    /// The Rust callback to invoke when AppKit fires the action selector.
    callback: Option<Box<dyn Fn()>>,
}

impl std::fmt::Debug for ActionTargetIvars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionTargetIvars").field("callback", &self.callback.is_some()).finish()
    }
}

impl Default for ActionTargetIvars {
    fn default() -> Self {
        Self { callback: None }
    }
}

define_class!(
    // SAFETY:
    // - Superclass `NSObject` imposes no subclassing requirements.
    // - `ActionTarget` does not implement `Drop` in a way that conflicts with ObjC's dealloc.
    // - This class is only ever used from the main thread (AppKit actions fire on main thread).
    #[unsafe(super(NSObject))]
    #[ivars = ActionTargetIvars]
    struct ActionTarget;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ActionTarget {}

    impl ActionTarget {
        /// ObjC action selector called by NSButton when clicked.
        /// Signature matches AppKit's target-action convention: `-(void)action:(id)sender`.
        // SAFETY: The selector and signature match what we register on each NSButton.
        #[unsafe(method(buttonClicked:))]
        fn button_clicked(&self, _sender: *mut AnyObject) {
            if let Some(cb) = self.ivars().callback.as_ref() {
                cb();
            }
        }
    }
);

impl ActionTarget {
    /// Allocate and initialise an `ActionTarget` on `mtm` with the given callback.
    fn new(_mtm: MainThreadMarker, callback: Box<dyn Fn()>) -> Retained<Self> {
        // `AnyThread::alloc()` takes no arguments for non-MainThreadOnly classes.
        let this = Self::alloc().set_ivars(ActionTargetIvars { callback: Some(callback) });
        // SAFETY: `NSObject`'s `init` has no preconditions beyond being called on an allocated
        // instance, which `set_ivars` returns.
        unsafe { msg_send![super(this), init] }
    }
}

// ─── Public entry point ───────────────────────────────────────────────────────────────────────

/// Install a native macOS titlebar on `window`.
///
/// Call this **before** `window.present()`: it registers a `connect_realize` handler,
/// and `present()` is what triggers realize. The actual AppKit work happens inside that
/// handler, which runs on the main thread once the surface (and its NSWindow) exists.
///
/// `primary` — if `true`, adds the server-address settings button; if `false`, no accessory
/// button is added (the native green traffic-light button handles maximize/fullscreen).
pub fn install(
    window: &gtk4::ApplicationWindow,
    primary: bool,
    addr: &ServerAddr,
    writer: &Writer,
) {
    let addr = addr.clone();
    let writer = writer.clone();
    let window_weak = window.downgrade();

    window.connect_realize(move |_realized_widget| {
        let Some(win) = window_weak.upgrade() else { return };
        install_on_realized(&win, primary, &addr, &writer);
    });
}

/// Called from inside `connect_realize` — the surface is guaranteed to exist here.
fn install_on_realized(
    window: &gtk4::ApplicationWindow,
    primary: bool,
    addr: &ServerAddr,
    writer: &Writer,
) {
    // ── 1. Reach the GdkSurface raw pointer ──────────────────────────────────────────
    let Some(surface) = window.surface() else {
        tracing::warn!("native_titlebar: window surface is None after realize — no native titlebar");
        return;
    };
    // `as_ptr()` (from glib::prelude::ObjectExt, via gtk4::prelude::*) gives us the raw
    // *mut GdkSurface which is layout-compatible with gpointer / *mut c_void.
    // SAFETY: `surface` is a live GObject reference; its raw pointer is valid for the
    // duration of this call (we don't cross any thread boundary).
    let raw_surface: *mut c_void = surface.as_ptr() as *mut c_void;

    if raw_surface.is_null() {
        tracing::warn!("native_titlebar: GdkSurface raw pointer is null — no native titlebar");
        return;
    }

    // ── 2. Get the NSWindow pointer from GDK ─────────────────────────────────────────
    // SAFETY: `raw_surface` is a valid GdkMacosSurface* (on macOS, every toplevel GdkSurface
    // from gtk4-macos is a GdkMacosSurface). The function is exported from libgtk-4.dylib and
    // returns a raw NSWindow* pointer (or null if not yet realized, which we checked above).
    let ns_win_ptr = unsafe { gdk_macos_surface_get_native_window(raw_surface) };
    if ns_win_ptr.is_null() {
        tracing::warn!("native_titlebar: gdk_macos_surface_get_native_window returned null");
        return;
    }

    // SAFETY: `ns_win_ptr` is a valid, live ObjC `NSWindow *` — the same object managed by
    // GTK's macOS backend. The backend keeps it alive for the window's lifetime.  We take a
    // shared reference (&NSWindow) and never store it beyond this function.
    let ns_window: &NSWindow = unsafe { &*(ns_win_ptr as *const NSWindow) };

    // ── 3. Diagnostics — style mask BEFORE ───────────────────────────────────────────
    let mask_before = ns_window.styleMask();
    let transparent_before = ns_window.titlebarAppearsTransparent();
    tracing::info!(
        "native_titlebar: NSWindow style mask BEFORE = {:#010x} \
         (Titled={} Closable={} Miniaturizable={} Resizable={} FullScreen={})",
        mask_before.0,
        mask_before.contains(NSWindowStyleMask::Titled),
        mask_before.contains(NSWindowStyleMask::Closable),
        mask_before.contains(NSWindowStyleMask::Miniaturizable),
        mask_before.contains(NSWindowStyleMask::Resizable),
        mask_before.contains(NSWindowStyleMask::FullScreen),
    );
    tracing::info!(
        "native_titlebar: titlebarAppearsTransparent BEFORE = {transparent_before}"
    );

    // ── 4. Configure the native titlebar ─────────────────────────────────────────────
    // Ensure Titled is set (GTK might have set only Borderless).
    let new_mask = mask_before | NSWindowStyleMask::Titled;
    ns_window.setStyleMask(new_mask);

    let mask_after = ns_window.styleMask();
    tracing::info!(
        "native_titlebar: NSWindow style mask AFTER  = {:#010x} \
         (Titled={} Closable={} Miniaturizable={} Resizable={} FullScreen={})",
        mask_after.0,
        mask_after.contains(NSWindowStyleMask::Titled),
        mask_after.contains(NSWindowStyleMask::Closable),
        mask_after.contains(NSWindowStyleMask::Miniaturizable),
        mask_after.contains(NSWindowStyleMask::Resizable),
        mask_after.contains(NSWindowStyleMask::FullScreen),
    );

    // Title visible, not transparent.
    ns_window.setTitleVisibility(NSWindowTitleVisibility::Visible);
    ns_window.setTitlebarAppearsTransparent(false);
    tracing::info!(
        "native_titlebar: titlebarAppearsTransparent AFTER = {}",
        ns_window.titlebarAppearsTransparent()
    );

    // ── 5. Build the accessory (settings button, primary window only) ─────────────────
    // No custom fullscreen/maximize button: the native green traffic-light button already
    // provides that on macOS. The only accessory button is the server-address settings
    // button, which lives on the primary window. Non-primary windows get no accessory
    // (an empty accessory would just reserve dead titlebar space).
    if primary {
        // We need a MainThreadMarker to call many AppKit APIs.
        // SAFETY: we are on the GTK main thread, which is the macOS main thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };

        let vc = NSTitlebarAccessoryViewController::new(mtm);
        // Trailing (right) placement in the titlebar.
        vc.setLayoutAttribute(NSLayoutAttribute::Right);

        // Container view (NSView) holding the single settings button.
        // Frame: 32px button + 8px trailing padding = 40px; height = 28px (standard titlebar).
        let container_frame = NSRect {
            origin: objc2_foundation::NSPoint { x: 0.0, y: 0.0 },
            size: NSSize { width: 40.0, height: 28.0 },
        };
        let container = NSView::initWithFrame(NSView::alloc(mtm), container_frame);

        let settings_image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            ns_string!("network"),
            Some(ns_string!("Server address")),
        );

        let window_clone = window.clone();
        let addr_clone = addr.clone();
        let writer_clone = writer.clone();
        let settings_callback: Box<dyn Fn()> = Box::new(move || {
            crate::show_server_addr_dialog(&window_clone, &addr_clone, &writer_clone);
        });
        let settings_target = ActionTarget::new(mtm, settings_callback);

        let settings_btn = make_button(mtm, settings_image.as_deref(), "⚙", &settings_target);
        settings_btn.setFrame(NSRect {
            origin: objc2_foundation::NSPoint { x: 4.0, y: 0.0 },
            size: NSSize { width: 32.0, height: 28.0 },
        });
        container.addSubview(&settings_btn);

        // Keep the action target alive. Since we `forget` it here, it is never deallocated —
        // fine here (it lives as long as the app / window).
        // SAFETY: `Retained<ActionTarget>` is intentionally leaked; that leaked +1 is what
        // keeps the object alive. AppKit's `target` is an unretained (assign) reference and
        // does NOT own it, so the leak — not the button — is what prevents a use-after-free.
        std::mem::forget(settings_target);

        // Attach the accessory VC to the window.
        vc.setView(&container);
        ns_window.addTitlebarAccessoryViewController(&vc);
    }

    // TODO(spike): native FPS label — add an NSTextField accessory, updated from the 1s timer.

    // ── 6. Diagnostics ────────────────────────────────────────────────────────────────
    // Count of titlebar accessory VCs (ours, if primary, plus any GTK added).
    let vc_count = ns_window.titlebarAccessoryViewControllers().count();
    tracing::info!(
        "native_titlebar: titlebarAccessoryViewControllers count after install = {vc_count}"
    );
    tracing::info!(
        "native_titlebar: install complete (primary={primary}). \
         Check logs for double-titlebar diagnosis — see report."
    );
}

/// Create an `NSButton` for the titlebar accessory.
///
/// Uses an SF Symbol image if `image` is `Some`; otherwise falls back to a text title.
/// The button's `target` is set to `target` (as `AnyObject`) and `action` to `buttonClicked:`.
fn make_button(
    mtm: MainThreadMarker,
    image: Option<&NSImage>,
    fallback_title: &str,
    target: &ActionTarget,
) -> Retained<NSButton> {
    let action_sel = sel!(buttonClicked:);
    // SAFETY: `target` is a valid NSObject subclass instance on the main thread. `action_sel`
    // is registered by `define_class!` above and matches the method we defined.
    let btn = if let Some(img) = image {
        unsafe {
            NSButton::buttonWithImage_target_action(
                img,
                Some(target.as_ref()),
                Some(action_sel),
                mtm,
            )
        }
    } else {
        let title = NSString::from_str(fallback_title);
        unsafe {
            NSButton::buttonWithTitle_target_action(
                &title,
                Some(target.as_ref()),
                Some(action_sel),
                mtm,
            )
        }
    };
    btn
}
