//! macOS fullscreen presentation policy: keep the Mac menu bar — and with it the window's
//! titlebar — from sliding down over the top of a fullscreen viewer window.
//!
//! # Why
//! In macOS fullscreen the menu bar is only *auto-hidden* by default: parking the pointer at
//! the top edge slides the menu bar and the window's titlebar down over the content. Both
//! live outside GDK's `NSWindow` (the menu bar is a system window; the titlebar rides in
//! AppKit's fullscreen overlay window), so the pointer leaves the GDK surface there — and
//! GDK's macOS backend then stops delivering motion until the next button release inside
//! the window. Observed as: mouse motion stops reaching the remote in the top ~50px of a
//! fullscreen window, and stays stopped until you click somewhere below that strip. Two GDK
//! paths can hold the stall (`GdkMacosWindow.inMove`, cleared only by an `NSLeftMouseUp` in
//! that window's `sendEvent:`; and events tagged with the foreign overlay window, which
//! `_gdk_macos_display_translate` hands straight to AppKit) — whichever one fires, both need
//! the reveal to happen first.
//!
//! A remote-desktop viewer wants the opposite of the reveal anyway: the clone's own top bar
//! (GNOME's) lives in exactly that strip, and the operator needs the pointer to reach it. So
//! in fullscreen we ask AppKit for `HideMenuBar | HideDock` instead of the auto-hide pair:
//! nothing is revealed at the top edge, the pointer never leaves the surface, and F11
//! (handled locally in `install_keyboard`) remains the way out.
//!
//! # How
//! The sanctioned knob is the `NSWindowDelegate` method
//! `window:willUseFullScreenPresentationOptions:`. GDK's `GdkMacosWindow` is its own delegate
//! and does not implement it, so we add the method to that class at runtime with
//! `class_addMethod` — **before any window exists**, because AppKit snapshots which delegate
//! methods are implemented at `setDelegate:` time, which `GdkMacosWindow` does in its
//! initializer. Nothing else about GDK's window is touched.
//!
//! Opt out (keep the stock auto-hide reveal): `RMNG_FULLSCREEN_MENUBAR=1`.

use objc2::ffi;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2_app_kit::NSApplicationPresentationOptions;
use objc2_foundation::NSUInteger;

/// Rewrite AppKit's proposed fullscreen presentation options: swap the auto-hide menu bar /
/// Dock pair for the hidden pair and keep everything else — notably `FullScreen`, which
/// AppKit requires to stay set. `HideMenuBar` must be accompanied by `HideDock` (AppKit
/// rejects it with an auto-hidden Dock), hence both are forced together.
fn hidden_menubar_options(
    proposed: NSApplicationPresentationOptions,
) -> NSApplicationPresentationOptions {
    (proposed
        - (NSApplicationPresentationOptions::AutoHideMenuBar
            | NSApplicationPresentationOptions::AutoHideDock))
        | NSApplicationPresentationOptions::HideMenuBar
        | NSApplicationPresentationOptions::HideDock
}

/// The injected delegate method:
/// `-(NSApplicationPresentationOptions)window:(NSWindow *)window
///   willUseFullScreenPresentationOptions:(NSApplicationPresentationOptions)proposedOptions`.
/// AppKit calls it on the main thread as the window enters fullscreen.
unsafe extern "C-unwind" fn will_use_fullscreen_presentation_options(
    _this: *mut AnyObject,
    _cmd: Sel,
    _window: *mut AnyObject,
    proposed: NSUInteger,
) -> NSUInteger {
    let proposed = NSApplicationPresentationOptions::from_bits_retain(proposed);
    let chosen = hidden_menubar_options(proposed);
    tracing::info!(
        "fullscreen: presentation options {proposed:?} → {chosen:?} \
         (Mac menu bar hidden, not auto-hidden; F11 leaves fullscreen)"
    );
    chosen.bits()
}

/// Install the policy on `GdkMacosWindow`. Call once, before GTK creates any window.
///
/// Every failure mode is logged and leaves GDK's stock behaviour in place (the menu bar
/// auto-hides, and the top-edge motion stall comes back) — never a panic.
pub fn install() {
    if std::env::var_os("RMNG_FULLSCREEN_MENUBAR").is_some() {
        tracing::info!(
            "fullscreen: RMNG_FULLSCREEN_MENUBAR set — keeping the Mac menu bar's auto-hide reveal"
        );
        return;
    }
    let Some(cls) = AnyClass::get(c"GdkMacosWindow") else {
        tracing::warn!(
            "fullscreen: GdkMacosWindow class not found — the Mac menu bar will auto-hide in fullscreen"
        );
        return;
    };
    let sel = Sel::register(c"window:willUseFullScreenPresentationOptions:");
    if cls.instance_method(sel).is_some() {
        // A future GDK that grows its own policy wins; say so, since the stall may be back.
        tracing::warn!(
            "fullscreen: GdkMacosWindow already implements window:willUseFullScreenPresentationOptions: \
             — leaving GDK's policy in place"
        );
        return;
    }

    // SAFETY: an `Imp` is an untyped `extern "C-unwind" fn()`; the runtime calls it with the
    // signature described by the type encoding below (`Q@:@Q`: NSUInteger return, then
    // self, _cmd, the NSWindow, and the proposed NSUInteger options), which is exactly the
    // signature of `will_use_fullscreen_presentation_options`.
    let imp: Imp = unsafe {
        std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, NSUInteger) -> NSUInteger,
            Imp,
        >(will_use_fullscreen_presentation_options)
    };
    // SAFETY: `cls` is a live, registered class (the runtime owns it for the process
    // lifetime); `class_addMethod` only mutates it if the selector is not yet implemented,
    // which we checked above; the encoding string is a valid NUL-terminated C string.
    let added = unsafe {
        ffi::class_addMethod(std::ptr::from_ref(cls).cast_mut(), sel, imp, c"Q@:@Q".as_ptr())
    };
    if added.as_bool() {
        tracing::info!(
            "fullscreen: installed window:willUseFullScreenPresentationOptions: on GdkMacosWindow \
             (Mac menu bar hidden in fullscreen; RMNG_FULLSCREEN_MENUBAR=1 restores the reveal)"
        );
    } else {
        tracing::warn!(
            "fullscreen: class_addMethod on GdkMacosWindow failed — the Mac menu bar will auto-hide in fullscreen"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use NSApplicationPresentationOptions as O;

    /// AppKit's stock proposal for a fullscreen window.
    fn stock() -> O {
        O::FullScreen | O::AutoHideMenuBar | O::AutoHideDock
    }

    #[test]
    fn swaps_the_auto_hide_pair_for_the_hidden_pair() {
        let got = hidden_menubar_options(stock());
        assert!(got.contains(O::HideMenuBar | O::HideDock));
        assert!(!got.intersects(O::AutoHideMenuBar | O::AutoHideDock));
    }

    #[test]
    fn keeps_fullscreen_set() {
        // AppKit requires FullScreen to survive the delegate's answer.
        assert!(hidden_menubar_options(stock()).contains(O::FullScreen));
    }

    #[test]
    fn preserves_unrelated_bits() {
        let got = hidden_menubar_options(stock() | O::AutoHideToolbar);
        assert!(got.contains(O::AutoHideToolbar));
    }

    #[test]
    fn hidden_menubar_always_brings_hidden_dock() {
        // HideMenuBar with an auto-hidden Dock is an invalid AppKit combination.
        let got = hidden_menubar_options(O::FullScreen | O::AutoHideMenuBar);
        assert!(got.contains(O::HideDock));
        assert!(!got.contains(O::AutoHideDock));
    }
}
