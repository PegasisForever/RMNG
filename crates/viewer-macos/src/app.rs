//! The AppKit main thread: application setup, the net-thread → main-thread wake hop, window
//! reconciliation from the server's `ViewSpec`, draw-on-frame, and the housekeeping tick. The GTK
//! viewer's `build_ui` + 8 ms tick, re-expressed on NSApplication with no GTK.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSMenu,
    NSMenuItem, NSTextField, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSTimer,
};

use dispatch2::DispatchQueue;
use viewer_core::auto_lock::{lock_action, LockAction};
use viewer_core::config;
use wire::viewer::{ViewContent, ViewMonitor};
use crate::shared::send_tagged;

use crate::clipboard::Clipboard;
use crate::cursor::cursor_from_shape;
use crate::pointer::PointerLock;
use crate::render::{Overlay, Renderer};
use crate::shared::{Shared, Wake};
use crate::terminal::{TermCallbacks, TerminalView};
use crate::window::{
    install_close_policy, is_main_monitor, make_video_view, make_window_shell, ViewerView, WinCtx,
};

/// How often the housekeeping tick runs: auto pointer-lock reconcile, cursor shape, clipboard,
/// focus loss. Matches the GTK viewer's 8 ms tick closely enough for the lock debounce.
const TICK_SECS: f64 = 0.016;

/// What a window currently shows. The shell outlives every content swap.
enum Content {
    /// A headed clone's desktop for this monitor.
    Video { view: Retained<ViewerView>, ctx: Rc<WinCtx> },
    /// The tmux tab view — only ever on the main window (id 0). `clone` is the owning headless
    /// clone: when the selection moves to a different one the view is rebuilt, so one clone's
    /// `main` tab and its scrollback can never be reused for another's.
    Terminal { clone: String, view: TerminalView },
    /// A secondary window while a headless clone is selected: kept open, nothing to paint.
    Placeholder,
}

/// One live monitor window.
struct WindowEntry {
    monitor_id: u32,
    window: Retained<NSWindow>,
    content: Content,
    /// Whether this window was the key window at the previous tick (to detect focus loss).
    was_key: Cell<bool>,
    /// The agent-cursor sprite as a Metal texture, and the shape version it was built from.
    overlay_tex: RefCell<Option<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>>>>,
    overlay_version: Cell<u64>,
}

impl WindowEntry {
    fn video(&self) -> Option<(&Retained<ViewerView>, &Rc<WinCtx>)> {
        match &self.content {
            Content::Video { view, ctx } => Some((view, ctx)),
            _ => None,
        }
    }
}

/// Everything the main thread owns. Lives in a main-thread-only `thread_local`; `wake` reaches it
/// by dispatching `on_main_wake` onto the main queue.
struct AppState {
    mtm: MainThreadMarker,
    shared: Arc<Shared>,
    renderer: Renderer,
    windows: HashMap<u32, WindowEntry>,
    last_epoch: u64,
    startup: Option<Retained<NSWindow>>,
    cmd_is_ctrl: bool,
    pointer_lock: Option<Rc<PointerLock>>,
    clipboard: Clipboard,
    /// Target for menu and tab-strip actions (AppKit holds targets unretained).
    delegate: Retained<Delegate>,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// Coalesce wake bursts: only one `on_main_wake` is in flight at a time.
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);

/// Net-thread wake: schedule a main-thread refresh (reconcile + draw). Cheap and coalesced.
pub fn wake(_why: Wake) {
    if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
        DispatchQueue::main().exec_async(on_main_wake);
    }
}

fn on_main_wake() {
    WAKE_PENDING.store(false, Ordering::Release);
    with_state(|s| s.refresh());
}

fn with_state(f: impl FnOnce(&mut AppState)) {
    APP.with(|a| {
        if let Some(state) = a.borrow_mut().as_mut() {
            f(state);
        }
    });
}

impl AppState {
    /// Reconcile the window set to the latest spec, then draw whatever frames are available.
    fn refresh(&mut self) {
        let (spec, epoch) = {
            let v = self.shared.view.lock().unwrap();
            (v.spec.clone(), v.epoch)
        };
        if epoch != self.last_epoch {
            self.last_epoch = epoch;
            let monitors: Vec<ViewMonitor> =
                spec.as_ref().map(|s| s.monitors.clone()).unwrap_or_default();
            self.reconcile(&monitors, spec.as_ref().map(|s| &s.content));
        }
        self.draw_all();
    }

    /// Reconcile the window set and each window's content to the spec. Windows are created and
    /// destroyed only when the monitor set changes; switching clones just swaps content.
    fn reconcile(&mut self, monitors: &[ViewMonitor], content: Option<&ViewContent>) {
        let live: std::collections::HashSet<u32> = monitors.iter().map(|m| m.id).collect();
        for id in self.windows.keys().copied().filter(|id| !live.contains(id)).collect::<Vec<_>>() {
            if let Some(e) = self.windows.remove(&id) {
                if let Some((view, _)) = e.video() {
                    view.release_all();
                }
                e.window.close();
            }
        }
        let (terminal_mode, term_clone, sessions) = match content {
            Some(ViewContent::Terminal { clone, sessions }) => {
                (true, clone.clone(), sessions.clone())
            }
            _ => (false, String::new(), Vec::new()),
        };
        // A terminal clone has no desktop pointer, and a vanished window can strand the lock —
        // it is process-wide, and on macOS holding it with no target freezes the host cursor.
        if terminal_mode || self.windows.is_empty() {
            if let Some(pl) = self.pointer_lock.as_ref() {
                pl.release();
            }
        }

        for m in monitors {
            if !self.windows.contains_key(&m.id) {
                let title = format!("RMNG viewer — monitor {}", m.id);
                let window = make_window_shell(self.mtm, &title, is_main_monitor(m.id));
                self.windows.insert(
                    m.id,
                    WindowEntry {
                        monitor_id: m.id,
                        window,
                        content: Content::Placeholder,
                        was_key: Cell::new(false),
                        overlay_tex: RefCell::new(None),
                        overlay_version: Cell::new(0),
                    },
                );
            }
            let want_terminal = terminal_mode && m.id == 0;
            let want_placeholder = terminal_mode && m.id != 0;
            let e = self.windows.get_mut(&m.id).expect("just inserted / already present");

            if want_terminal {
                // Rebuild only when the owning clone changes, so a re-sent spec keeps scrollback.
                let same = matches!(&e.content, Content::Terminal { clone, .. } if *clone == term_clone);
                if !same {
                    let frame = e.window.contentView().map(|v| v.bounds()).unwrap_or(NSRect::new(
                        NSPoint::new(0.0, 0.0),
                        NSSize::new(1280.0, 720.0),
                    ));
                    let view = TerminalView::new(self.mtm, term_callbacks(&self.shared), frame);
                    // AppKit targets are unretained; the delegate lives as long as the app.
                    unsafe {
                        view.tabs().setTarget(Some(&self.delegate));
                        view.tabs().setAction(Some(objc2::sel!(rmngTabClicked:)));
                    }
                    e.window.setContentView(Some(view.view()));
                    e.window.makeFirstResponder(Some(&**view.grid_view()));
                    e.content = Content::Terminal { clone: term_clone.clone(), view };
                }
                if let Content::Terminal { view, .. } = &e.content {
                    view.set_sessions(&sessions);
                }
            } else if want_placeholder {
                if !matches!(e.content, Content::Placeholder) {
                    let label = NSTextField::labelWithString(
                        ns_string!("Headless clone selected — no desktop"),
                        self.mtm,
                    );
                    e.window.setContentView(Some(&label));
                    e.content = Content::Placeholder;
                }
            } else if !matches!(e.content, Content::Video { .. }) {
                let ctx = Rc::new(WinCtx {
                    monitor_id: m.id,
                    shared: self.shared.clone(),
                    writer: self.shared.writer.clone(),
                    cmd_is_ctrl: self.cmd_is_ctrl,
                    pointer_lock: self.pointer_lock.clone(),
                    frame_size: Cell::new((m.width as f64, m.height as f64)),
                    pressed: RefCell::new(Default::default()),
                    buttons: RefCell::new(Default::default()),
                    cursor: RefCell::new(None),
                    cursor_version: Cell::new(0),
                    inside: Cell::new(false),
                });
                let view =
                    make_video_view(self.mtm, &e.window, ctx.clone(), self.renderer.device());
                e.content = Content::Video { view, ctx };
            }
        }

        // Keep-alive / status window when there are no content windows at all.
        if self.windows.is_empty() {
            if self.startup.is_none() {
                self.startup = Some(make_startup_window(self.mtm));
            }
        } else if let Some(w) = self.startup.take() {
            w.close();
        }
    }

    /// Draw the latest frame for each window (latest-wins; a window with no frame yet clears).
    fn draw_all(&mut self) {
        let frames = self.shared.frames.lock().unwrap();
        let cursors = self.shared.cursors.lock().unwrap();
        let now = Instant::now();
        for e in self.windows.values() {
            let Some((view, ctx)) = e.video() else { continue };
            let layer = view.metal_layer();
            let bounds = view.bounds();
            let scale = view.window().map(|w| w.backingScaleFactor()).unwrap_or(2.0);
            let (dw, dh) = (bounds.size.width * scale, bounds.size.height * scale);
            if dw < 1.0 || dh < 1.0 {
                continue;
            }
            layer.setDrawableSize(NSSize::new(dw, dh));
            let Some(drawable) = layer.nextDrawable() else { continue };
            let frame = frames.get(&e.monitor_id);
            if let Some(f) = frame {
                ctx.frame_size.set((f.width as f64, f.height as f64));
            }
            // The synthetic cursor is drawn ONLY while the remote agent is driving this
            // monitor's pointer, so the operator can see where it is going; the rest of the time
            // the real OS cursor (wearing the remote's shape) is the only one on screen.
            let overlay = cursors.get(&e.monitor_id).and_then(|c| {
                if !c.warp_until.is_some_and(|d| now < d) {
                    return None;
                }
                let shape = c.shape.as_ref()?;
                if e.overlay_version.get() != c.version || e.overlay_tex.borrow().is_none() {
                    match self.renderer.cursor_texture(
                        &shape.rgba,
                        shape.width as usize,
                        shape.height as usize,
                    ) {
                        Ok(t) => {
                            *e.overlay_tex.borrow_mut() = Some(t);
                            e.overlay_version.set(c.version);
                        }
                        Err(err) => {
                            tracing::warn!("monitor {}: overlay sprite failed: {err:#}", e.monitor_id);
                            return None;
                        }
                    }
                }
                let texture = e.overlay_tex.borrow().clone()?;
                Some(Overlay {
                    texture,
                    x: (c.x - shape.hotspot_x as i32) as f64,
                    y: (c.y - shape.hotspot_y as i32) as f64,
                    w: shape.width as f64,
                    h: shape.height as f64,
                })
            });
            if let Err(err) = self.renderer.draw(frame, &drawable, dw, dh, overlay.as_ref()) {
                tracing::warn!("monitor {}: draw error: {err:#}", e.monitor_id);
            }
        }
    }

    /// Housekeeping: focus loss, auto pointer-lock, the remote cursor shape, and the clipboard.
    fn tick(&mut self) {
        // 1. Focus loss releases every key/button this window holds, so nothing sticks down on
        //    the remote after a Cmd+Tab away.
        let mut has_target = false;
        for e in self.windows.values() {
            let is_key = e.window.isKeyWindow();
            if e.was_key.get() && !is_key {
                if let Some((view, _)) = e.video() {
                    view.release_all();
                }
            }
            e.was_key.set(is_key);
            has_target |= is_key && e.video().is_some();
        }

        // 2. Auto pointer-lock: remote cursor hidden ≥180 ms engages, shown ≥300 ms releases;
        //    the manual chords override on top. Wanting the lock with no focused window is a
        //    RELEASE condition — a held macOS lock outlives our focus and would freeze the host
        //    cursor inside whatever app the operator switched to.
        if let Some(pl) = self.pointer_lock.as_ref() {
            let want = self.shared.auto_lock.lock().unwrap().want(Instant::now());
            match lock_action(want, has_target, pl.is_engaged()) {
                LockAction::Engage => pl.engage(),
                LockAction::Release => pl.release(),
                LockAction::Nothing => {}
            }
        }

        // 3. Remote cursor shape → a real NSCursor, rebuilt only when the sprite changes.
        {
            let cursors = self.shared.cursors.lock().unwrap();
            for e in self.windows.values() {
                let Some((_, ctx)) = e.video() else { continue };
                let Some(entry) = cursors.get(&e.monitor_id) else { continue };
                if entry.version == ctx.cursor_version.get() {
                    continue;
                }
                let Some(shape) = entry.shape.as_ref() else { continue };
                ctx.cursor_version.set(entry.version);
                match cursor_from_shape(shape) {
                    Ok(c) => {
                        tracing::debug!(
                            "cursor apply: mon={} version={} {}x{}",
                            e.monitor_id, entry.version, shape.width, shape.height
                        );
                        *ctx.cursor.borrow_mut() = Some(c);
                        ctx.apply_cursor();
                    }
                    // Keep the previous cursor rather than losing the pointer shape entirely.
                    Err(err) => tracing::warn!("monitor {}: cursor build failed: {err:#}", e.monitor_id),
                }
            }
        }

        // 4. Terminal: feed the server's PTY bytes into the tab that owns them, and let the view
        //    settle its grid size (debounced, so a live window drag sends one resize, not many).
        let chunks: Vec<(String, Vec<u8>)> =
            self.shared.term_out.lock().unwrap().drain(..).collect();
        for e in self.windows.values() {
            if let Content::Terminal { view, .. } = &e.content {
                for (session, data) in &chunks {
                    view.feed(session, data);
                }
                view.tick();
            }
        }

        // 5. Clipboard: drain inbound offers/data and notice a local copy.
        let shared = self.shared.clone();
        self.clipboard.tick(&shared);
    }
}

fn on_main_tick() {
    with_state(|s| s.tick());
}

/// Open the server-address dialog (⌘, or the menu item).
fn open_settings() {
    let addr_changed = APP.with(|a| {
        let b = a.borrow();
        let Some(state) = b.as_ref() else { return false };
        let (mtm, shared) = (state.mtm, state.shared.clone());
        // Drop the borrow before running a modal loop: the dialog pumps events, which can
        // re-enter the tick and would panic on a second borrow_mut.
        drop(b);
        crate::settings::show(mtm, &shared)
    });
    if addr_changed {
        wake(Wake::View);
    }
}

/// Route the terminal view's input, resize and new-session events back to the server. The
/// viewer→server tags mirror the GTK viewer: 3 = keystrokes, 4 = grid size, 5 = new session.
fn term_callbacks(shared: &Arc<Shared>) -> TermCallbacks {
    TermCallbacks {
        on_input: {
            let shared = shared.clone();
            Rc::new(move |session: &str, data: Vec<u8>| {
                let msg = wire::viewer::TermInput { session: session.to_string(), data };
                if let Ok(json) = serde_json::to_string(&msg) {
                    send_tagged(&shared.writer, 3, &json);
                }
            })
        },
        on_resize: {
            let shared = shared.clone();
            Rc::new(move |cols: u16, rows: u16| {
                if let Ok(json) = serde_json::to_string(&wire::viewer::TermResize { cols, rows }) {
                    send_tagged(&shared.writer, 4, &json);
                }
            })
        },
        on_new_session: {
            let shared = shared.clone();
            Rc::new(move || {
                if let Ok(json) = serde_json::to_string(&wire::viewer::TermNewSession {}) {
                    send_tagged(&shared.writer, 5, &json);
                }
            })
        },
    }
}

/// The startup / keep-alive window: a small titled window with a status label, shown until video
/// arrives. The address is editable from the app menu (⌘,) — the reason the GTK viewer puts a
/// Settings button here is that with a wrong address no other window ever appears.
fn make_startup_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(440.0, 150.0));
    let style =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("RMNG viewer"));
    // The startup window is the only UI before the first spec arrives, so it is the main window
    // for close purposes: closing it quits, rather than hiding the viewer behind the menu bar.
    install_close_policy(mtm, &window, true);
    let label = NSTextField::labelWithString(
        ns_string!("Connecting to the server…\nChange the address with ⌘, (Settings)."),
        mtm,
    );
    label.setFrame(NSRect::new(NSPoint::new(20.0, 45.0), NSSize::new(400.0, 60.0)));
    if let Some(content) = window.contentView() {
        content.addSubview(&label);
    }
    window.center();
    window.makeKeyAndOrderFront(None);
    window
}

// ── application delegate ────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct DelegateIvars;

define_class!(
    // SAFETY: NSObject superclass has no subclassing requirements; no conflicting Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateIvars]
    #[name = "RmngAppDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            // Windows going away must not kill the app; the viewer is driven by the server's
            // view spec and can legitimately have no window for a while. Quitting on a user
            // close is the window delegate's job instead (see `install_close_policy`).
            false
        }
    }

    impl Delegate {
        /// Menu action for Settings (⌘,).
        #[unsafe(method(rmngShowSettings:))]
        fn rmng_show_settings(&self, _sender: *mut objc2::runtime::AnyObject) {
            open_settings();
        }

        /// The terminal tab strip was clicked (a session tab, or the trailing "+").
        #[unsafe(method(rmngTabClicked:))]
        fn rmng_tab_clicked(&self, _sender: *mut objc2::runtime::AnyObject) {
            with_state(|s| {
                for e in s.windows.values() {
                    if let Content::Terminal { view, .. } = &e.content {
                        view.on_tab_clicked();
                    }
                }
            });
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars);
        unsafe { msg_send![super(this), init] }
    }
}

/// Build the app menu: Settings (⌘,) and Quit (⌘Q).
fn install_menu(mtm: MainThreadMarker, app: &NSApplication, delegate: &Delegate) {
    let main = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    main.addItem(&app_item);

    let app_menu = NSMenu::new(mtm);
    let settings = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Settings…"),
            Some(objc2::sel!(rmngShowSettings:)),
            ns_string!(","),
        )
    };
    unsafe { settings.setTarget(Some(delegate)) };
    app_menu.addItem(&settings);
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Quit RMNG viewer"),
            Some(objc2::sel!(terminate:)),
            ns_string!("q"),
        )
    };
    app_menu.addItem(&quit);
    app_item.setSubmenu(Some(&app_menu));
    app.setMainMenu(Some(&main));
}

/// Run the GUI: build the app state, install it in the main-thread thread-local, show the startup
/// window, start the housekeeping tick, and enter the AppKit run loop.
pub fn run(shared: Arc<Shared>) -> Result<()> {
    let mtm =
        MainThreadMarker::new().ok_or_else(|| anyhow!("run() must be called on the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let delegate = Delegate::new(mtm);
    install_menu(mtm, &app, &delegate);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let renderer = Renderer::new()?;
    let pointer_lock = PointerLock::new(shared.writer.clone()).map(Rc::new);
    let mut state = AppState {
        mtm,
        shared: shared.clone(),
        renderer,
        windows: HashMap::new(),
        last_epoch: 0,
        startup: None,
        cmd_is_ctrl: config::cmd_is_ctrl(),
        pointer_lock,
        clipboard: Clipboard::new(mtm),
        delegate: delegate.clone(),
    };
    // Show the startup window immediately: the net thread may connect before the first spec.
    state.startup = Some(make_startup_window(mtm));
    APP.with(|a| *a.borrow_mut() = Some(state));

    // Housekeeping tick (the wake hop covers frames; this covers everything time-driven).
    let block = RcBlock::new(|_timer: std::ptr::NonNull<NSTimer>| on_main_tick());
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(TICK_SECS, true, &block) };

    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
    Ok(())
}
