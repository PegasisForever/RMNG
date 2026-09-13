//! Native window regression check; requires a logged-in macOS desktop.
//! cargo run -p viewer-macos --example window-layout-check -- -AppleWindowTabbingMode fullscreen
//! Add --windowed to check an ordinary window; use "always" to force the strongest tab preference.
#![allow(dead_code)]

#[path = "../src/decoder.rs"]
mod decoder;
#[path = "../src/pointer.rs"]
mod pointer;
#[path = "../src/shared.rs"]
mod shared;
#[path = "../src/window.rs"]
mod window;

use std::time::{Duration, Instant};

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask, NSWindowStyleMask};
use objc2_foundation::{MainThreadMarker, NSDate, NSDefaultRunLoopMode};

fn pump(app: &NSApplication, seconds: f64) {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
        if let Some(event) = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&until),
                NSDefaultRunLoopMode,
                true,
            )
        } {
            app.sendEvent(&event);
        }
        app.updateWindows();
    }
}

fn main() {
    let fullscreen = !std::env::args().any(|arg| arg == "--windowed");
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    let first = window::make_window_shell(mtm, "Layout check — monitor 0");
    pump(&app, 0.5);
    if fullscreen {
        first.toggleFullScreen(None);
        pump(&app, 2.0);
        assert!(
            first.styleMask().contains(NSWindowStyleMask::FullScreen),
            "first window must be fullscreen"
        );
    }

    // The same constructor called by AppState::reconcile when a layout adds a monitor.
    let second = window::make_window_shell(mtm, "Layout check — monitor 1");
    pump(&app, 1.0);
    let first_tabs = first.tabbedWindows().map_or(0, |tabs| tabs.len());
    let second_tabs = second.tabbedWindows().map_or(0, |tabs| tabs.len());
    let still_fullscreen = first.styleMask().contains(NSWindowStyleMask::FullScreen);
    println!(
        "first tab group: {first_tabs}; second tab group: {second_tabs}; first still fullscreen: {still_fullscreen}"
    );

    // Leave the desktop as it was, including when the assertion below fails.
    if still_fullscreen {
        first.toggleFullScreen(None);
        pump(&app, 2.0);
    }
    second.close();
    first.close();
    assert!(
        first_tabs <= 1 && second_tabs <= 1,
        "new monitor became an OS tab instead of a separate window"
    );
    assert_eq!(
        still_fullscreen, fullscreen,
        "adding a monitor must preserve the existing window's fullscreen state"
    );
}
