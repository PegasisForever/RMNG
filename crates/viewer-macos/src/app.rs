//! The AppKit main thread: application setup, the net-thread → main-thread wake hop, window
//! reconciliation from the server's `ViewSpec`, draw-on-frame, and the housekeeping tick. The GTK
//! viewer's `build_ui` + 8 ms tick, re-expressed on NSApplication with no GTK.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSTextField,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSSize, NSTimer,
};

use dispatch2::DispatchQueue;
use viewer_core::auto_lock::{lock_action, LockAction};
use viewer_core::config;
use viewer_core::drag_route::Screen;
use wire::viewer::{ViewContent, ViewMonitor};
use crate::shared::send_tagged;

use crate::clipboard::Clipboard;
use crate::cursor::cursor_from_shape;
use crate::decoder::DecodedFrame;
use crate::pointer::PointerLock;
use crate::render::{Overlay, Renderer};
use crate::shared::{CursorEntry, Shared, Wake, WakeQueue, WakeSet};
use crate::terminal::{TermCallbacks, TerminalView};
use crate::window::{
    install_window_delegate, make_video_view, make_window_shell, SharedLayout, ViewerView, WinCtx,
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
    /// Whether the last draw put the agent cursor on screen. A frame is what normally erases it,
    /// so when the warp window expires on a still desktop this is what says "one more draw".
    overlay_drawn: Cell<bool>,
}

impl WindowEntry {
    fn video(&self) -> Option<(&Retained<ViewerView>, &Rc<WinCtx>)> {
        match &self.content {
            Content::Video { view, ctx } => Some((view, ctx)),
            _ => None,
        }
    }

    /// Drop everything this window's current content still holds down on the remote.
    ///
    /// Owed by every path that takes the content away, not just the ones that take the window
    /// away: the held keys and buttons live in the `WinCtx`, which a content swap drops on the
    /// floor. Select a headless clone with a key held and, without this, that key stays down on
    /// the remote with nothing left that could ever release it.
    fn release_input(&self) {
        if let Some((view, _)) = self.video() {
            view.release_all();
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
    /// The monitor rectangles every window routes a cross-seam drag against; refreshed from the
    /// spec on each reconcile and shared with every live `WinCtx`.
    layout: SharedLayout,
    last_epoch: u64,
    startup: Option<Retained<NSWindow>>,
    cmd_is_ctrl: bool,
    pointer_lock: Option<Rc<PointerLock>>,
    /// Whether pointer lock was engaged at the previous tick, to catch its release edge (the
    /// moment the remote's cursor shape has to be put back — see `tick`).
    was_locked: bool,
    clipboard: Clipboard,
    /// Target for menu and tab-strip actions (AppKit holds targets unretained).
    delegate: Retained<Delegate>,
    /// Keeps the ⌘Q / ⌘, event monitor installed for as long as the app runs; removing it is
    /// what would put those chords back on the menu (see [`install_menu_chord_monitor`]).
    _menu_chord_monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// Coalesce wake bursts — only one `on_main_wake` is in flight at a time — while keeping every
/// reason that arrived, so the refresh does only the work the wakes actually asked for.
static WAKES: WakeQueue = WakeQueue::new();

/// Net-thread wake: record why, and schedule a main-thread refresh if one isn't already coming.
pub fn wake(why: Wake) {
    if WAKES.push(why) {
        DispatchQueue::main().exec_async(on_main_wake);
    }
}

fn on_main_wake() {
    let wakes = WAKES.drain();
    let mut handled = false;
    with_state(|s| {
        s.refresh(&wakes);
        handled = true;
    });
    if !handled {
        // No app state yet. A wake can only land here once the run loop is pumping the main
        // queue, i.e. after `run()` installed the state, so this is belt and braces — but a
        // dropped reason means a monitor that never gets its frame drawn, so hand them back.
        WAKES.restore(&wakes);
    }
}

fn with_state(f: impl FnOnce(&mut AppState)) {
    APP.with(|a| {
        if let Some(state) = a.borrow_mut().as_mut() {
            f(state);
        }
    });
}

impl AppState {
    /// Do what the wakes asked for: reconcile on a spec change, feed the terminal on data, and
    /// present only the monitors with something new to show. Everything else on this path runs
    /// per decoded frame per monitor, so it stays off the GPU unless it has a reason to be there.
    fn refresh(&mut self, wakes: &WakeSet) {
        // The epoch, not `wakes.view`, decides the reconcile: it is the same test as before and
        // cannot miss a spec that landed while a wake was in flight. The spec is cloned only
        // when it actually moved.
        let (spec, epoch) = {
            let v = self.shared.view.lock().unwrap();
            if v.epoch == self.last_epoch {
                (None, v.epoch)
            } else {
                (v.spec.clone(), v.epoch)
            }
        };
        let reconciled = epoch != self.last_epoch;
        if reconciled {
            self.last_epoch = epoch;
            let monitors: Vec<ViewMonitor> =
                spec.as_ref().map(|s| s.monitors.clone()).unwrap_or_default();
            self.reconcile(&monitors, spec.as_ref().map(|s| &s.content));
        }

        // After the reconcile: a wake that both created the terminal window and carried its first
        // bytes must feed the window it just built.
        if wakes.data {
            self.feed_terminal();
        }

        if reconciled {
            // A reconcile can have built a window or swapped its content: paint every one of
            // them from the frames already in hand rather than leaving a new window blank until
            // the remote next repaints (a still desktop sends nothing).
            self.draw_all();
            return;
        }

        let mut ids = wakes.frames.clone();
        if wakes.data {
            // The agent cursor is a drawn overlay, and a warp moves it with no video frame
            // behind it, so a data wake still has to repaint the monitors it is on — and the one
            // draw after it expires, to take it off screen.
            for id in self.overlay_monitors(Instant::now()) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        if !ids.is_empty() {
            self.draw(&ids);
        }
    }

    /// Monitors whose agent-cursor overlay needs a repaint right now: the ones being driven, plus
    /// any that still have the sprite on screen after the warp window closed.
    fn overlay_monitors(&self, now: Instant) -> Vec<u32> {
        let cursors = self.shared.cursors.lock().unwrap();
        self.windows
            .values()
            .filter(|e| e.video().is_some())
            .filter(|e| {
                e.overlay_drawn.get()
                    || cursors
                        .get(&e.monitor_id)
                        .is_some_and(|c| c.warp_until.is_some_and(|d| now < d))
            })
            .map(|e| e.monitor_id)
            .collect()
    }

    /// Reconcile the window set and each window's content to the spec. Windows are created and
    /// destroyed only when the monitor set changes; switching clones just swaps content.
    fn reconcile(&mut self, monitors: &[ViewMonitor], content: Option<&ViewContent>) {
        // Drag-routing layout from the configured monitor geometry. Refreshed before anything
        // else touches the window set, so a window that survives this reconcile is already
        // routing against the new geometry.
        *self.layout.borrow_mut() = monitors
            .iter()
            .map(|m| Screen { id: m.id, x: m.x, y: m.y, w: m.width, h: m.height })
            .collect();

        let live: std::collections::HashSet<u32> = monitors.iter().map(|m| m.id).collect();
        for id in self.windows.keys().copied().filter(|id| !live.contains(id)).collect::<Vec<_>>() {
            if let Some(e) = self.windows.remove(&id) {
                e.release_input();
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
                let window = make_window_shell(self.mtm, &title);
                self.windows.insert(
                    m.id,
                    WindowEntry {
                        monitor_id: m.id,
                        window,
                        content: Content::Placeholder,
                        was_key: Cell::new(false),
                        overlay_tex: RefCell::new(None),
                        overlay_version: Cell::new(0),
                        overlay_drawn: Cell::new(false),
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
                    e.release_input();
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
                    e.release_input();
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
                    layout: self.layout.clone(),
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

    /// Draw every video window — after a reconcile, where the whole window set may be new.
    fn draw_all(&mut self) {
        let ids: Vec<u32> =
            self.windows.values().filter(|e| e.video().is_some()).map(|e| e.monitor_id).collect();
        self.draw(&ids);
    }

    /// Draw the latest frame for the given monitors (latest-wins; a monitor with no frame yet
    /// clears).
    ///
    /// The shared state is copied out first and both locks released before any layer or renderer
    /// call. `nextDrawable` blocks while the layer's drawable pool is empty — a vsync interval
    /// with display sync on, and up to its ~1 s timeout for an occluded or miniaturised window —
    /// and holding `frames`/`cursors` across it parks the decoder's output callback and the
    /// cursor latch, which is display pacing leaking into the socket reader.
    fn draw(&mut self, monitors: &[u32]) {
        let now = Instant::now();
        let inputs: Vec<(u32, Option<DecodedFrame>, Option<CursorEntry>)> = {
            let frames = self.shared.frames.lock().unwrap();
            let cursors = self.shared.cursors.lock().unwrap();
            monitors
                .iter()
                .map(|&id| {
                    // Copying the frame is a retain on the IOSurface-backed pixel buffer, and it
                    // keeps the buffer alive for this draw even if the decoder replaces the slot.
                    let frame = frames.get(&id).map(|f| DecodedFrame {
                        pixel_buffer: f.pixel_buffer.clone(),
                        width: f.width,
                        height: f.height,
                        yuv444: f.yuv444,
                    });
                    // Only the sprite the overlay is about to draw is worth copying, and only
                    // while the agent is driving this monitor.
                    let cursor = cursors
                        .get(&id)
                        .filter(|c| c.warp_until.is_some_and(|d| now < d))
                        .cloned();
                    (id, frame, cursor)
                })
                .collect()
        };

        for (id, frame, cursor) in &inputs {
            let Some(e) = self.windows.get(id) else { continue };
            let Some((view, ctx)) = e.video() else { continue };
            // Pointer lock owns the cursor: the real one is hidden and the remote is being
            // driven by relative motion, so the agent's sprite would be the only pointer on
            // screen and it would be pointing at a position nobody is using. The GTK viewer
            // gates the same overlay the same way (`let show = !locked && …`). Asked here
            // rather than in the snapshot above because that runs under the frame/cursor locks,
            // where the window's `WinCtx` is not in hand.
            let cursor = if ctx.locked() { None } else { cursor.as_ref() };
            let layer = view.metal_layer();
            let bounds = view.bounds();
            let scale = view.window().map(|w| w.backingScaleFactor()).unwrap_or(2.0);
            let (dw, dh) = (bounds.size.width * scale, bounds.size.height * scale);
            if dw < 1.0 || dh < 1.0 {
                continue;
            }
            layer.setDrawableSize(NSSize::new(dw, dh));
            let Some(drawable) = layer.nextDrawable() else { continue };
            if let Some(f) = frame {
                ctx.frame_size.set((f.width as f64, f.height as f64));
            }
            // The synthetic cursor is drawn ONLY while the remote agent is driving this
            // monitor's pointer, so the operator can see where it is going; the rest of the time
            // the real OS cursor (wearing the remote's shape) is the only one on screen.
            let overlay = cursor.and_then(|c| {
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
            match self.renderer.draw(frame.as_ref(), &drawable, dw, dh, overlay.as_ref()) {
                // The flag tracks what is on screen, so it moves only when something was
                // presented; a failed draw leaves the previous contents, overlay and all.
                Ok(()) => e.overlay_drawn.set(overlay.is_some()),
                Err(err) => tracing::warn!("monitor {}: draw error: {err:#}", e.monitor_id),
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
        // 2b. Put the remote's cursor shape back on the lock's release edge. Releasing un-hides
        //     the system cursor wearing whatever shape AppKit last set — the plain arrow — and
        //     `apply_cursor` otherwise only runs on entering a view or on a new sprite, so the
        //     remote's I-beam/hand would not come back until the pointer next crossed a window
        //     boundary. The GTK viewer flips the cursor on the same edge (`locked !=
        //     cursor_hidden`). Only the view the pointer is actually inside acts on it.
        let locked = self.pointer_lock.as_ref().is_some_and(|pl| pl.is_engaged());
        if self.was_locked && !locked {
            for e in self.windows.values() {
                if let Some((_, ctx)) = e.video() {
                    ctx.apply_cursor();
                }
            }
        }
        self.was_locked = locked;

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

        // 4. Terminal: the bytes themselves are fed on the wake that announced them (keystroke
        //    echo must not wait for a timer); this catch-up costs an empty drain and covers a
        //    wake that landed before the window existed. What is genuinely time-driven is the
        //    view's own tick: the grid-size debounce, so a live drag sends one resize, not many.
        self.feed_terminal();
        for e in self.windows.values() {
            if let Content::Terminal { view, .. } = &e.content {
                view.tick();
            }
        }

        // 5. Clipboard: drain inbound offers/data and notice a local copy. This one stays on the
        //    timer — noticing a local copy is a `changeCount` poll with no wake behind it, and
        //    clipboard round-trips do not care about a 16 ms hop.
        let shared = self.shared.clone();
        self.clipboard.tick(&shared);

        // 6. A resized window has to be re-presented at its new drawable size, and no wake says
        //    so: a still remote sends no frames, and the draw path now only touches monitors
        //    that have one. The comparison is two floats per window; the draw happens only when
        //    the size really moved, i.e. while the operator is dragging the frame.
        let resized: Vec<u32> = self
            .windows
            .values()
            .filter_map(|e| {
                let (view, _) = e.video()?;
                let bounds = view.bounds();
                let scale = view.window().map(|w| w.backingScaleFactor()).unwrap_or(2.0);
                let (dw, dh) = (bounds.size.width * scale, bounds.size.height * scale);
                let have = view.metal_layer().drawableSize();
                let stale = dw >= 1.0 && dh >= 1.0 && (dw != have.width || dh != have.height);
                stale.then_some(e.monitor_id)
            })
            .collect();
        if !resized.is_empty() {
            self.draw(&resized);
        }
    }

    /// Hand the server's PTY bytes to the tab that owns them. Driven by the data wake, so a
    /// keystroke echoes as soon as the socket reader has it.
    fn feed_terminal(&self) {
        let chunks: Vec<(String, Vec<u8>)> =
            self.shared.term_out.lock().unwrap().drain(..).collect();
        if chunks.is_empty() {
            return;
        }
        for e in self.windows.values() {
            if let Content::Terminal { view, .. } = &e.content {
                for (session, data) in &chunks {
                    view.feed(session, data);
                }
            }
        }
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
    // The startup window is the only UI before the first spec arrives, so closing it quits like
    // any monitor window does, rather than hiding the viewer behind the menu bar.
    install_window_delegate(mtm, &window);
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
            // close is the window delegate's job instead (see `install_window_delegate`).
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

/// Carbon kVKs of the two keys the app menu claims as ⌘-equivalents.
const KVK_Q: u32 = 0x0C;
const KVK_COMMA: u32 = 0x2B;

/// Let ⌘Q and ⌘, reach the remote instead of the menu.
///
/// AppKit runs a menu item's key equivalent from `sendEvent:`, *before* the event is offered to
/// the first responder — so with the Cmd↔Ctrl swap on, the two chords [`install_menu`] claims
/// could never be typed at the remote as Ctrl+Q / Ctrl+, and ⌘Q killed the viewer mid-session.
/// An `NSEvent` local monitor runs earlier still (it sees the event before `sendEvent:` is
/// called at all), which is exactly how the GTK viewer reads the keyboard on macOS — see
/// `crates/viewer/src/keyboard_macos.rs`.
///
/// It is focus-aware and deliberately narrow. The chords are taken only while a video view is
/// the first responder of the key window *and* the swap is on — i.e. only when the remote is
/// listening for them. A terminal window, the startup window and the settings dialog keep the
/// stock ⌘Q and ⌘,, and both menu items stay clickable everywhere, so the viewer never becomes
/// impossible to quit. With the swap off, Cmd is not standing in for the remote's Ctrl and the
/// chords carry no remote meaning worth taking the local ones for.
///
/// A stolen event is *consumed* and handed to the view here, so it is delivered exactly once —
/// passing it through instead would hand it straight back to the menu, which is the bug.
fn install_menu_chord_monitor() -> Option<Retained<AnyObject>> {
    let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a live event of one of the masked types.
        let ev = unsafe { event.as_ref() };
        let kvk = ev.keyCode() as u32;
        if (kvk != KVK_Q && kvk != KVK_COMMA)
            || !ev.modifierFlags().contains(NSEventModifierFlags::Command)
        {
            return event.as_ptr();
        }
        let mut target: Option<Retained<ViewerView>> = None;
        with_state(|s| {
            if !s.cmd_is_ctrl {
                return;
            }
            target = s
                .windows
                .values()
                .filter(|e| e.window.isKeyWindow())
                .find_map(|e| e.video().map(|(view, _)| view.clone()))
                .filter(|view| view.owns_keystrokes());
        });
        // The state borrow is released before dispatching: `keyDown:` runs the whole forwarding
        // path, and re-entering `with_state` from under it would panic on the second borrow.
        let Some(view) = target else { return event.as_ptr() };
        if ev.r#type() == NSEventType::KeyUp {
            // macOS withholds `keyUp:` from the responder chain while Cmd is held, so the
            // release has to come from here too or Q would stay down on the remote forever.
            view.keyUp(ev);
        } else {
            view.keyDown(ev);
        }
        std::ptr::null_mut()
    });
    // SAFETY: called on the main thread, where the block also runs; the handler returns either
    // the event it was given or null, which is the contract the monitor requires.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::KeyDown | NSEventMask::KeyUp,
            &block,
        )
    };
    if monitor.is_none() {
        // Not fatal: the viewer keeps working, the two chords just stay local (⌘Q quits).
        tracing::warn!("⌘Q/⌘, monitor install failed; those chords will not reach the remote");
    }
    monitor
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
        layout: Rc::new(RefCell::new(Vec::new())),
        last_epoch: 0,
        startup: None,
        cmd_is_ctrl: config::cmd_is_ctrl(),
        pointer_lock,
        was_locked: false,
        clipboard: Clipboard::new(mtm),
        delegate: delegate.clone(),
        _menu_chord_monitor: install_menu_chord_monitor(),
    };
    // Show the startup window immediately: the net thread may connect before the first spec.
    state.startup = Some(make_startup_window(mtm));
    APP.with(|a| *a.borrow_mut() = Some(state));

    // Housekeeping tick (the wake hop covers frames; this covers everything time-driven).
    // Added to the run loop by hand in the common modes rather than scheduled: the convenience
    // constructor registers in the default mode only, so a live window resize or an open menu —
    // which run the loop in event-tracking mode — would stop the tick exactly when the auto
    // pointer-lock reconcile and the terminal's resize debounce are needed most.
    let block = RcBlock::new(|_timer: std::ptr::NonNull<NSTimer>| on_main_tick());
    // SAFETY: the block outlives the timer (it is copied by `timerWithTimeInterval…`) and runs
    // on the run loop that owns the timer, i.e. this main thread.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(TICK_SECS, true, &block) };
    // A quarter-interval of slop lets the kernel coalesce the wake-up with other timers; nothing
    // here is deadline-driven at finer than tick granularity.
    timer.setTolerance(TICK_SECS * 0.25);
    // SAFETY: called on the main thread, so `currentRunLoop` is the run loop `app.run()` drives;
    // the run loop retains the timer, which is what keeps it alive past this scope.
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };

    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
    Ok(())
}
