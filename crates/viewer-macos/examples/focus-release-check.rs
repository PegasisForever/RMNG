//! Native focus-loss regression check; requires a logged-in macOS desktop.
//! cargo run -p viewer-macos --example focus-release-check
#![allow(dead_code)]
#[path = "../src/decoder.rs"]
mod decoder;
#[path = "../src/pointer.rs"]
mod pointer;
#[path = "../src/shared.rs"]
mod shared;
#[path = "../src/window.rs"]
mod window;

use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSPoint, NSString};
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn pump(app: &NSApplication) {
    let deadline = Instant::now() + Duration::from_millis(300);
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

fn held_chord(view: &window::ViewerView, win: &NSWindow) {
    let cmd = NSEventModifierFlags(NSEventModifierFlags::Command.0 | 0x8);
    let empty = NSString::from_str("");
    let v = NSString::from_str("v");
    let modifier = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(NSEventType::FlagsChanged, NSPoint::new(0.0, 0.0), cmd, 0.0, win.windowNumber(), None, &empty, &empty, false, 0x37).unwrap();
    let key = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(NSEventType::KeyDown, NSPoint::new(0.0, 0.0), cmd, 0.0, win.windowNumber(), None, &v, &v, false, 0x09).unwrap();
    view.flagsChanged(&modifier);
    view.keyDown(&key);
    let mouse = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDown, NSPoint::new(100.0, 100.0), cmd, 0.0, win.windowNumber(), None, 1, 1, 1.0,
    ).unwrap();
    view.mouseDown(&mouse);
}

fn drain(stream: &mut TcpStream) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    loop {
        let mut header = [0u8; 5];
        match stream.read_exact(&mut header) {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(e) => panic!("input header: {e}"),
        }
        assert_eq!(header[0], 0);
        let mut body = vec![0; u32::from_be_bytes(header[1..].try_into().unwrap()) as usize];
        stream.read_exact(&mut body).unwrap();
        result.push(serde_json::from_slice(&body).unwrap());
    }
    result
}

fn assert_released(events: &[serde_json::Value]) {
    // Key releases may be in either order; each held key/button must have exactly one press
    // and one release, including when the window's next housekeeping tick sees focus restored.
    for (kind, field, code) in [
        ("key_code", "keycode", 29),
        ("key_code", "keycode", 47),
        ("button", "button", 272),
    ] {
        let edges: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == kind && e[field] == code)
            .map(|e| e["pressed"].as_bool().unwrap())
            .collect();
        assert_eq!(
            edges,
            [true, false],
            "{kind} {code} must be released once on focus loss"
        );
    }
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut reader, _) = listener.accept().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let outgoing = shared::Writer::default();
    outgoing.connect(writer).unwrap();
    let shared = Arc::new(shared::Shared {
        writer: outgoing,
        addr: Arc::new(Mutex::new(String::new())),
        chroma: 0.into(),
        connected: false.into(),
        view: Default::default(),
        cursors: Default::default(),
        warp: Default::default(),
        auto_lock: Mutex::new(viewer_core::auto_lock::AutoLock::new(Instant::now())),
        clip_inbox: Default::default(),
        term_out: Default::default(),
        frames: Default::default(),
        forwards: Arc::new(viewer_core::forward::ForwardManager::new(Arc::new(|_| {}))),
        wake: Box::new(|_| {}),
    });
    let ctx = Rc::new(window::WinCtx {
        monitor_id: 0,
        writer: shared.writer.clone(),
        shared,
        layout: Default::default(),
        cmd_is_ctrl: true,
        pointer_lock: None,
        frame_size: Cell::new((1280.0, 720.0)),
        pressed: RefCell::new(Default::default()),
        buttons: Default::default(),
        cursor: Default::default(),
        cursor_version: Cell::new(0),
        inside: Cell::new(false),
    });
    let win = window::make_window_shell(mtm, "Focus release check");
    let device = objc2_metal::MTLCreateSystemDefaultDevice().unwrap();
    let view = window::make_video_view(mtm, &win, ctx.clone(), &device);
    pump(&app);
    assert!(win.isKeyWindow());
    held_chord(&view, &win);
    assert!(
        ctx.pressed.borrow().contains(&47),
        "V forwarded before focus switch"
    );
    assert!(
        ctx.buttons.borrow().contains(&272),
        "mouse press forwarded before focus switch"
    );
    let other = window::make_window_shell(mtm, "Focus release target");
    assert!(other.isKeyWindow() && !win.isKeyWindow());
    let held_at_loss = ctx.pressed.borrow().clone();
    win.makeKeyAndOrderFront(None);
    assert!(win.isKeyWindow());
    let held_after_return = ctx.pressed.borrow().clone();
    let buttons_after_return = ctx.buttons.borrow().clone();
    let window_events = drain(&mut reader);
    view.release_all();
    held_chord(&view, &win);
    assert!(win.makeFirstResponder(None));
    assert!(!view.owns_keystrokes());
    let held_after_responder_loss = ctx.pressed.borrow().clone();
    let buttons_after_responder_loss = ctx.buttons.borrow().clone();
    let responder_events = drain(&mut reader);
    view.release_all();
    other.close();
    win.close();
    println!(
        "keys held at key-window loss: {held_at_loss:?}; after focus return without tick: {held_after_return:?}; after responder loss: {held_after_responder_loss:?}"
    );
    assert!(
        held_at_loss.is_empty()
            && held_after_return.is_empty()
            && held_after_responder_loss.is_empty(),
        "focus loss leaves remote Ctrl+V held; sampled focus can miss the loss"
    );
    assert!(
        buttons_after_return.is_empty() && buttons_after_responder_loss.is_empty(),
        "focus loss leaves remote mouse button held"
    );
    assert_released(&window_events);
    assert_released(&responder_events);
    println!("both focus-loss paths released Ctrl, V, and mouse button exactly once on the wire");
}
