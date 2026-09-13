//! Native keyboard dispatch regression check; requires a logged-in macOS desktop.
//! cargo run -p viewer-macos --example keyboard-release-check
#![allow(dead_code)]
#[path = "../src/decoder.rs"]
mod decoder;
#[path = "../src/pointer.rs"]
mod pointer;
#[path = "../src/shared.rs"]
mod shared;
#[path = "../src/window.rs"]
mod window;

use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSMenu, NSMenuItem, NSWindow,
};
use objc2_foundation::{
    MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSObject, NSObjectProtocol, NSPoint, NSString,
};
use shared::{Shared, Writer};
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use viewer_core::{auto_lock::AutoLock, forward::ForwardManager};
use window::WinCtx;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RmngKeyboardCheckMenuTarget"]
    #[ivars = Cell<u32>]
    struct MenuTarget;
    unsafe impl NSObjectProtocol for MenuTarget {}
    impl MenuTarget {
        #[unsafe(method(checkMenuAction:))]
        fn menu_action(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            self.ivars().set(self.ivars().get() + 1);
        }
    }
);

fn install_menu(mtm: MainThreadMarker, app: &NSApplication) -> objc2::rc::Retained<MenuTarget> {
    let target = MenuTarget::alloc(mtm).set_ivars(Cell::new(0));
    let target: objc2::rc::Retained<MenuTarget> = unsafe { msg_send![super(target), init] };
    let menu = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    let submenu = NSMenu::new(mtm);
    for key in ["q", ","] {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Keyboard check action"),
                Some(objc2::sel!(checkMenuAction:)),
                &NSString::from_str(key),
            )
        };
        unsafe {
            item.setTarget(Some(&target));
        }
        submenu.addItem(&item);
    }
    app_item.setSubmenu(Some(&submenu));
    menu.addItem(&app_item);
    app.setMainMenu(Some(&menu));
    target
}

fn pump(app: &NSApplication, seconds: f64) {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.01);
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

fn context(writer: Writer, cmd_is_ctrl: bool) -> Rc<WinCtx> {
    let shared = Arc::new(Shared {
        writer: writer.clone(),
        addr: Arc::new(Mutex::new(String::new())),
        chroma: Default::default(),
        connected: Default::default(),
        view: Default::default(),
        cursors: Default::default(),
        warp: Default::default(),
        auto_lock: Mutex::new(AutoLock::new(Instant::now())),
        clip_inbox: Default::default(),
        term_out: Default::default(),
        frames: Default::default(),
        forwards: Arc::new(ForwardManager::new(Arc::new(|_| {}))),
        wake: Box::new(|_| {}),
    });
    Rc::new(WinCtx {
        monitor_id: 0,
        shared,
        layout: Rc::new(RefCell::new(Vec::new())),
        writer,
        cmd_is_ctrl,
        pointer_lock: None,
        frame_size: Cell::new((1280.0, 720.0)),
        pressed: Default::default(),
        buttons: Default::default(),
        cursor: Default::default(),
        cursor_version: Cell::new(0),
        inside: Cell::new(false),
    })
}

fn send(
    app: &NSApplication,
    win: &NSWindow,
    ty: NSEventType,
    code: u16,
    flags: usize,
    chars: &str,
) {
    send_with_repeat(app, win, ty, code, flags, chars, false);
}

fn send_with_repeat(
    app: &NSApplication,
    win: &NSWindow,
    ty: NSEventType,
    code: u16,
    flags: usize,
    chars: &str,
    repeat: bool,
) {
    let chars = NSString::from_str(chars);
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        ty, NSPoint::new(0.0, 0.0), NSEventModifierFlags(flags), 0.0, win.windowNumber(), None, &chars, &chars, repeat, code,
    ).expect("keyboard event");
    app.postEvent_atStart(&event, false);
    pump(app, 0.01);
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
                break
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

fn key_events(stream: &mut TcpStream) -> Vec<(u64, bool)> {
    drain(stream)
        .into_iter()
        .map(|ev| {
            assert_eq!(ev["kind"], "key_code");
            (
                ev["keycode"].as_u64().unwrap(),
                ev["pressed"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn main() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    // Local monitors run while AppKit dequeues events, so dispatch through its real queue.
    // --without-monitor retains the original deterministic failure for diagnosis.
    let without_monitor = std::env::args().any(|a| a == "--without-monitor");
    let _monitor = if without_monitor {
        None
    } else {
        Some(window::install_keyboard_monitor(mtm).expect("keyboard monitor"))
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let writer_stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut reader, _) = listener.accept().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let writer = Writer::default();
    writer.connect(writer_stream).unwrap();
    let device = objc2_metal::MTLCreateSystemDefaultDevice().expect("Metal device");
    let menu_target = install_menu(mtm, &app);
    let mut failures = Vec::new();
    for swap in [true, false] {
        let ctx = context(writer.clone(), swap);
        let win = window::make_window_shell(mtm, "Keyboard release check");
        let view = window::make_video_view(mtm, &win, ctx.clone(), &device);
        pump(&app, 0.2);
        assert!(
            win.isKeyWindow() && view.owns_keystrokes(),
            "video must own keyboard"
        );
        for (cmd_kvk, device_flag, remote_cmd) in [
            (0x37, 0x8, if swap { 29 } else { 125 }),
            (0x36, 0x10, if swap { 97 } else { 126 }),
        ] {
            let cmd = NSEventModifierFlags::Command.0 | device_flag;
            for (kvk, chars, remote_key) in [(0x09, "v", 47), (0x0C, "q", 16), (0x2B, ",", 51)] {
                if without_monitor && chars != "v" {
                    continue;
                }
                if !swap && chars != "v" {
                    let before = menu_target.ivars().get();
                    send(&app, &win, NSEventType::FlagsChanged, cmd_kvk, cmd, "");
                    send(&app, &win, NSEventType::KeyDown, kvk, cmd, chars);
                    send(&app, &win, NSEventType::KeyUp, kvk, cmd, chars);
                    send(&app, &win, NSEventType::FlagsChanged, cmd_kvk, 0, "");
                    assert_eq!(
                        menu_target.ivars().get(),
                        before + 1,
                        "swap off keeps native menu shortcut"
                    );
                    assert_eq!(
                        key_events(&mut reader),
                        vec![(remote_cmd, true), (remote_cmd, false)]
                    );
                    continue;
                }
                for cmd_first in [false, true] {
                    let menu_before = menu_target.ivars().get();
                    send(&app, &win, NSEventType::FlagsChanged, cmd_kvk, cmd, "");
                    send(&app, &win, NSEventType::KeyDown, kvk, cmd, chars);
                    assert_eq!(
                        menu_target.ivars().get(),
                        menu_before,
                        "remote chord must bypass menu"
                    );
                    // Holding the chord remains a remote hold; local repeats must not add downs.
                    assert!(ctx.pressed.borrow().contains(&(remote_key as u32)));
                    send_with_repeat(&app, &win, NSEventType::KeyDown, kvk, cmd, chars, true);
                    if cmd_first {
                        send(&app, &win, NSEventType::FlagsChanged, cmd_kvk, 0, "");
                        send(&app, &win, NSEventType::KeyUp, kvk, 0, chars);
                    } else {
                        send(&app, &win, NSEventType::KeyUp, kvk, cmd, chars);
                        if ctx.pressed.borrow().contains(&(remote_key as u32)) {
                            failures.push(format!("swap={swap}, Cmd={cmd_kvk:#x}, key={chars}: letter stays held while Command is held (repeating chord)"));
                        }
                        send(&app, &win, NSEventType::FlagsChanged, cmd_kvk, 0, "");
                    }
                    let events = key_events(&mut reader);
                    let mut expected = vec![(remote_cmd, true), (remote_key, true)];
                    if cmd_first {
                        expected.extend([(remote_cmd, false), (remote_key, false)]);
                    } else {
                        expected.extend([(remote_key, false), (remote_cmd, false)]);
                    }
                    if events != expected || !ctx.pressed.borrow().is_empty() {
                        failures.push(format!("swap={swap}, Cmd={cmd_kvk:#x}, key={chars}, Command released first={cmd_first}: expected {expected:?}, got {events:?}, held={:?}", ctx.pressed.borrow()));
                    }
                    ctx.release_all_input();
                    drain(&mut reader);
                }
            }
        }
        // Ordinary held keys still use the server's repeat; AppKit's repeats are discarded.
        send(&app, &win, NSEventType::KeyDown, 0x00, 0, "a");
        send_with_repeat(&app, &win, NSEventType::KeyDown, 0x00, 0, "a", true);
        assert!(ctx.pressed.borrow().contains(&30));
        send(&app, &win, NSEventType::KeyUp, 0x00, 0, "a");
        assert_eq!(key_events(&mut reader), vec![(30, true), (30, false)]);
        assert!(ctx.pressed.borrow().is_empty());
        // A native text field must receive its own keys without remote events from the monitor.
        let field = objc2_app_kit::NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        win.setContentView(Some(&field));
        win.makeFirstResponder(Some(&field));
        assert!(!view.owns_keystrokes());
        send(&app, &win, NSEventType::KeyDown, 0x00, 0, "a");
        send(&app, &win, NSEventType::KeyUp, 0x00, 0, "a");
        let cmd = NSEventModifierFlags::Command.0 | 0x8;
        send(&app, &win, NSEventType::KeyUp, 0x09, cmd, "v");
        assert!(
            key_events(&mut reader).is_empty(),
            "native text input must not reach remote"
        );
        win.close();
    }
    for failure in &failures {
        eprintln!("FAIL: {failure}");
    }
    assert!(
        failures.is_empty(),
        "{} keyboard release failures",
        failures.len()
    );
    println!("PASS: 16 remote Command chords and 4 native menu chords, both release orders, both Command keys, swap on/off, local repeats, and native responder isolation");
}
