//! The AppKit main thread: application setup, the net-thread → main-thread wake hop, window
//! reconciliation from the server's `ViewSpec`, and draw-on-frame. The GTK viewer's `build_ui` +
//! tick, re-expressed on NSApplication with no GTK.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSMenu, NSMenuItem, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
};

use dispatch2::DispatchQueue;
use viewer_core::config;
use wire::viewer::{ViewContent, ViewMonitor};

use crate::render::Renderer;
use crate::shared::{Shared, Wake};
use crate::window::{make_window, ViewerView, WinCtx};

/// One live monitor window.
struct WindowEntry {
    monitor_id: u32,
    _window: Retained<NSWindow>,
    view: Retained<ViewerView>,
    ctx: Rc<WinCtx>,
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
    APP.with(|a| {
        if let Some(state) = a.borrow_mut().as_mut() {
            state.refresh();
        }
    });
}

impl AppState {
    /// Reconcile the window set to the latest spec, then draw whatever frames are available.
    fn refresh(&mut self) {
        let (spec, epoch, connected) = {
            let v = self.shared.view.lock().unwrap();
            (v.spec.clone(), v.epoch, self.shared.connected.load(Ordering::Relaxed))
        };
        if epoch != self.last_epoch {
            self.last_epoch = epoch;
            let monitors: Vec<ViewMonitor> = spec.as_ref().map(|s| s.monitors.clone()).unwrap_or_default();
            let desktop = matches!(spec.as_ref().map(|s| &s.content), Some(ViewContent::Desktop));
            self.reconcile(&monitors, desktop);
        }
        let _ = connected;
        self.draw_all();
    }

    /// Create/destroy monitor windows to match the spec (Desktop mode only for now; Terminal is a
    /// later milestone). An empty spec tears them down and shows the startup window.
    fn reconcile(&mut self, monitors: &[ViewMonitor], desktop: bool) {
        let live: std::collections::HashSet<u32> =
            if desktop { monitors.iter().map(|m| m.id).collect() } else { Default::default() };
        // Drop windows no longer present.
        let gone: Vec<u32> = self.windows.keys().copied().filter(|id| !live.contains(id)).collect();
        for id in gone {
            if let Some(e) = self.windows.remove(&id) {
                e.view.release_all();
                e._window.close();
            }
        }
        if desktop {
            for m in monitors {
                if self.windows.contains_key(&m.id) {
                    continue;
                }
                let ctx = Rc::new(WinCtx {
                    monitor_id: m.id,
                    shared: self.shared.clone(),
                    writer: self.shared.writer.clone(),
                    cmd_is_ctrl: self.cmd_is_ctrl,
                    frame_size: std::cell::Cell::new((m.width as f64, m.height as f64)),
                    pressed: RefCell::new(Default::default()),
                    buttons: RefCell::new(Default::default()),
                });
                let title = format!("RMNG viewer — monitor {}", m.id);
                let (window, view) = make_window(self.mtm, ctx.clone(), self.renderer.device(), &title);
                self.windows.insert(m.id, WindowEntry { monitor_id: m.id, _window: window, view, ctx });
            }
        }
        // Keep-alive / status window when there are no monitor windows.
        if self.windows.is_empty() {
            if self.startup.is_none() {
                self.startup = Some(make_startup_window(self.mtm, &self.shared));
            }
        } else if let Some(w) = self.startup.take() {
            w.close();
        }
    }

    /// Draw the latest frame for each window (latest-wins; a window with no frame yet clears).
    fn draw_all(&mut self) {
        let frames = self.shared.frames.lock().unwrap();
        for e in self.windows.values() {
            let layer = e.view.metal_layer();
            let bounds = e.view.bounds();
            let scale = e.view.window().map(|w| w.backingScaleFactor()).unwrap_or(2.0);
            let (dw, dh) = (bounds.size.width * scale, bounds.size.height * scale);
            if dw < 1.0 || dh < 1.0 {
                continue;
            }
            layer.setDrawableSize(NSSize::new(dw, dh));
            let Some(drawable) = layer.nextDrawable() else { continue };
            let frame = frames.get(&e.monitor_id);
            if let Some(f) = frame {
                e.ctx.frame_size.set((f.width as f64, f.height as f64));
            }
            if let Err(err) = self.renderer.draw(frame, &drawable, dw, dh) {
                tracing::warn!("monitor {}: draw error: {err:#}", e.monitor_id);
            }
        }
    }
}

/// The startup / keep-alive window: a small titled window with a status label, shown until video
/// arrives. (Settings dialog + live status text land in a later milestone.)
fn make_startup_window(mtm: MainThreadMarker, _shared: &Arc<Shared>) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, 160.0));
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
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
    let label =
        objc2_app_kit::NSTextField::labelWithString(ns_string!("Connecting to the server…"), mtm);
    label.setFrame(NSRect::new(NSPoint::new(20.0, 60.0), NSSize::new(380.0, 40.0)));
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
            // The main (monitor 0) window closing quits; secondary/startup windows do not force it.
            false
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars);
        unsafe { msg_send![super(this), init] }
    }
}

/// Build the minimal main menu (an app menu with Quit ⌘Q).
fn install_menu(mtm: MainThreadMarker, app: &NSApplication) {
    let main = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    main.addItem(&app_item);
    let app_menu = NSMenu::new(mtm);
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

/// Run the GUI. Builds the app state, installs it in the main-thread thread-local, shows the
/// startup window, and enters the AppKit run loop.
pub fn run(shared: Arc<Shared>) -> Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow!("run() must be called on the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    install_menu(mtm, &app);

    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let renderer = Renderer::new()?;
    let mut state = AppState {
        mtm,
        shared: shared.clone(),
        renderer,
        windows: HashMap::new(),
        last_epoch: 0,
        startup: None,
        cmd_is_ctrl: config::cmd_is_ctrl(),
    };
    // Show the startup window immediately (net thread may connect before the first spec).
    state.startup = Some(make_startup_window(mtm, &shared));
    APP.with(|a| *a.borrow_mut() = Some(state));

    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
    Ok(())
}
