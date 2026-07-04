//! `viewer` (Phase 5) — native client.
//!
//! Modes:
//!   - default (GUI): **one GTK4 window per remote monitor** (`monitor_id`). Each decodes
//!     VA-API H.264 via `vah264dec ! glupload ! gtk4paintablesink` → zero-copy `GdkPaintable`
//!     (portable incl. Intel). Input capture → port 1. A drag that leaves one window's edge
//!     (held button → implicit pointer grab → overshoot coords) is routed onto the
//!     neighbouring monitor via a left-to-right `Layout` + `route_drag` (ported from the old
//!     `../gtk` client), so a remote window-drag continues across the local-window seam.
//!   - `--headless`: decode + report per-monitor fps (CI driver). `RMNG_DUMP=*.png`
//!     writes the first decoded frame as PNG, then exits.
//!
//! Port-1 framing: `[u8 tag]` then tag 0 = `[u32be monitor_id][u32be len][AnnexB AU]`
//! (video), tag 1 = `[u32be len][JSON ClipboardMsg]`, tag 2 = `[u32be len][JSON CursorMeta]`.
//! viewer→server: `[u8 tag][u32be len][json]` (0 input, 1 clipboard, 2 forward-status). Auto-reconnects.
//!
//! The server address (`host:port`) is read from `~/.config/rmng-viewer/config.json`
//! and editable at runtime via the main window's title-bar Settings button (see
//! [`config`]); `RMNG_VIDEO` only seeds the default on first run, before any config
//! file exists. Headless mode (`--headless`) still reads `RMNG_VIDEO` directly.
//! A startup window with the same Settings button is shown from launch until the
//! first monitor window exists, so the address can be fixed without a connection.
//!
//!   viewer [--headless]
//!
//! `gtk4paintablesink`'s paintable is a GTK object (`!Send`), so all pipelines, paintables
//! and widgets live on the GTK main thread; the net thread only ships AU bytes over a queue.

mod config;
mod forward;
mod glunpack;
mod headless;
// The Wayland pointer-lock implementation only compiles (and links) on Linux.
// The macOS twin lives in pointer_lock_macos.rs (§4.5).
// Other platforms get a no-op stub.
#[cfg(target_os = "linux")]
mod pointer_lock;
#[cfg(target_os = "macos")]
#[path = "pointer_lock_macos.rs"]
mod pointer_lock;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod pointer_lock {
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};

    use gtk4::gdk;

    /// Stub for non-Linux/macOS builds: always returns `None` from `new`.
    pub struct PointerLock;

    impl PointerLock {
        pub fn new(_display: &gdk::Display, _writer: Arc<Mutex<Option<TcpStream>>>) -> Option<Self> {
            None
        }
        pub fn is_engaged(&self) -> bool {
            false
        }
        pub fn engage(&self, _surface: &gdk::Surface) {}
        pub fn release(&self) {}
    }
}

// Carbon kVK → Linux evdev translation table (macOS only).
#[cfg(target_os = "macos")]
mod kvk_evdev;

// Physical-keyboard capture via a raw NSEvent monitor (macOS only): bypasses GDK's
// IM-mediated key events, which synthesize phantom keycode-0 presses. See keyboard_macos.rs.
#[cfg(target_os = "macos")]
mod keyboard_macos;

// Native macOS titlebar: replaces the GTK HeaderBar with NSWindow + NSButton accessories.
#[cfg(target_os = "macos")]
mod native_titlebar;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pointer_lock::PointerLock;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSrc;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use wire::ChromaMode;
use wire::socket::{
    ClipboardData, ClipboardMsg, ClipboardOffer, ClipboardRequest, CursorMeta, CursorShape, MonitorPlacement,
};
use wire::forward::{ForwardStatusMsg, ForwardsMsg};
use wire::viewer::ModeMsg;

fn main() -> Result<()> {
    // GTK's default GL renderer (`ngl`) and `vulkan` cache GdkTextures by identity and keep serving
    // a stale copy when gtk4paintablesink hands them the *same* GdkTexture for a recycled buffer
    // slot whose pixels changed — so an old frame from a few back reappears (worse when the window
    // downscales, since they cache a scaled intermediate; clean at ~1:1). The legacy `gl` renderer
    // doesn't cache that way: it re-samples the live texture every draw, so it's clean AND fast.
    // Empirically confirmed on this Intel/Mesa box: cairo=clean(slow), ngl/vulkan=stale, gl=clean.
    // Pin `gl` unless the user overrides. Must be set before GTK realizes its first surface; we're
    // still single-threaded here so set_var is sound.
    // macOS: the legacy `gl` renderer was removed in GTK ≥ 4.18; the pin is Linux-only.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GSK_RENDERER").is_none() {
        unsafe { std::env::set_var("GSK_RENDERER", "gl") };
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // `clip` (the clipboard bridge) logs debug by default: copy/paste-driven
                // only (sparse), and the go-to trail for cross-machine clipboard issues.
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,clip=debug")),
        )
        .init();
    gst::init()?;
    glunpack::register()?;
    // `--glunpack-validate [W H]`: offline GPU-unpack vs CPU-oracle pixel check (no server needed).
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--glunpack-validate") {
        let w = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(256);
        let h = args.get(pos + 2).and_then(|s| s.parse().ok()).unwrap_or(144);
        return glunpack::validate(w, h);
    }
    if args.iter().any(|a| a == "--headless") {
        return headless::run();
    }
    run_gui()
}

/// Inbound video AUs `(monitor_id, AnnexB)` shipped net-thread → GTK main thread. Only a
/// monitor's *first* AU(s) ride this queue (until its window exists); steady-state AUs go
/// straight to the appsrc from the net thread via [`VideoSrcs`].
type VideoAus = Arc<Mutex<VecDeque<(u32, Vec<u8>)>>>;
/// Cap on the bootstrap queue so a stalled GTK thread can't grow it unboundedly (drops
/// oldest). Steady-state AUs bypass this queue entirely (see [`VideoSrcs`]).
const AU_QUEUE_CAP: usize = 300;
/// `monitor_id → appsrc` for every built decode pipeline, shared net-thread ⇄ GTK main.
/// Lets the net thread push an AU straight into the decoder the instant it's read — no GTK
/// tick hop (which cost up to one 8 ms tick of latency per frame). `AppSrc` is a thread-safe
/// `GstElement` (`Send`+`Sync`); only the `!Send` sink paintable forces the main thread, and
/// it never crosses here. Populated by the tick when it lazily builds each monitor's window.
type VideoSrcs = Arc<Mutex<HashMap<u32, AppSrc>>>;

/// Active chroma mode, announced by the server's tag-4 handshake before any AU
/// (`0` = Yuv420, today's direct decode; `1` = Yuv444, the AVC444 `W×2H` stream needing
/// reconstruction). Process-global because it's server-wide and fixed per session; the
/// net thread sets it, `make_decoder` reads it when lazily building each monitor pipeline.
static VIEWER_CHROMA: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// Input/clipboard write half (None while disconnected).
type Writer = Arc<Mutex<Option<TcpStream>>>;
/// The server `host:port`, shared GTK main thread (Settings dialog writes) → net
/// thread (reads on each reconnect). Editable at runtime; persisted via [`config`].
type ServerAddr = Arc<Mutex<String>>;
/// Inbound clipboard messages, drained on the GUI thread (GTK clipboard ops run there).
type ClipInbox = Arc<Mutex<VecDeque<ClipboardMsg>>>;
fn is_text_mime(m: &str) -> bool {
    m.starts_with("text/plain") || m == "UTF8_STRING" || m == "TEXT"
}

/// Latest cursor state per monitor. The native OS cursor is shown normally; the synthetic
/// overlay is drawn ONLY while the remote agent is driving the pointer, i.e. while
/// `warp_until` is in the future (set/refreshed by each `warp:true` update). The shape
/// persists across position-only updates; `version` bumps on shape change so the GUI
/// re-textures lazily.
#[derive(Default, Clone)]
struct CursorEntry {
    x: i32,
    y: i32,
    shape: Option<CursorShape>,
    version: u64,
    /// Draw the synthetic cursor on this monitor until this instant (agent-driven move).
    warp_until: Option<Instant>,
}
type Cursors = Arc<Mutex<HashMap<u32, CursorEntry>>>;

/// Deadline until which the viewer suppresses sending local pointer motion, set when
/// a server-initiated cursor **warp** arrives (an MCP-driven move) so the user's mouse
/// doesn't immediately yank the cursor off the agent's target. Debounced: each warp
/// pushes the deadline out; suppression ends 0.5 s after the last warp. Shared
/// net-thread (sets) → GTK main thread (checks).
type WarpSuppress = Arc<Mutex<Option<Instant>>>;
/// How long to suppress local motion after a warp.
const WARP_SUPPRESS: Duration = Duration::from_millis(500);
/// How long the synthetic cursor stays drawn after an agent-driven (warp) move on a
/// monitor. Normally only the native OS cursor is shown; the overlay is drawn ONLY while
/// the agent drives the pointer, so the operator can see where it goes. Refreshed by each
/// warp, so it persists through a multi-step agent glide and hides this long after the last.
const AGENT_CURSOR_SHOW: Duration = Duration::from_millis(1000);

/// One monitor's place in the desktop layout (unified-desktop px). Populated from the
/// server's reported layout (the clone's real positions); falls back to a computed
/// left-to-right packing until the report arrives.
#[derive(Clone, Copy)]
struct Screen {
    id: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}
/// Shared monitor layout used for cross-window drag routing (main thread).
type SharedLayout = Rc<RefCell<Vec<Screen>>>;
/// The server's reported layout, shared net-thread → GTK main thread.
type ReportedLayout = Arc<Mutex<Vec<MonitorPlacement>>>;

fn run_gui() -> Result<()> {
    // Server address: persisted config is the source of truth (the Settings dialog
    // edits it live); `RMNG_VIDEO` only seeds the default on first run.
    let addr: ServerAddr = Arc::new(Mutex::new(config::load().server_addr));
    let aus: VideoAus = Arc::new(Mutex::new(VecDeque::new()));
    let writer: Writer = Arc::new(Mutex::new(None));
    // Port-forward manager: reports status back as port-1 tag-2 frames via `writer`.
    let fwd_mgr: Arc<forward::ForwardManager> = {
        let writer = writer.clone();
        let report: forward::StatusReport = Arc::new(move |msg: ForwardStatusMsg| {
            if let Ok(json) = serde_json::to_string(&msg) {
                send_tagged(&writer, 2, json);
            }
        });
        Arc::new(forward::ForwardManager::new(report))
    };
    let inbox: ClipInbox = Arc::new(Mutex::new(VecDeque::new()));
    let cursors: Cursors = Arc::new(Mutex::new(HashMap::new()));
    let reported: ReportedLayout = Arc::new(Mutex::new(Vec::new()));
    let warp: WarpSuppress = Arc::new(Mutex::new(None));
    let srcs: VideoSrcs = Arc::new(Mutex::new(HashMap::new()));

    // Net thread: reconnect loop; read [u8 tag][…] → video queue / clipboard / cursor / layout.
    {
        let (aus, srcs, writer, inbox, cursors, reported, warp, addr, fwd_mgr) =
            (aus.clone(), srcs.clone(), writer.clone(), inbox.clone(), cursors.clone(), reported.clone(), warp.clone(), addr.clone(), fwd_mgr.clone());
        std::thread::spawn(move || {
            loop {
                // Re-read the (possibly just-changed) address each reconnect, so the
                // Settings dialog can repoint us live: it updates `addr` and shuts the
                // current connection, the read below errors, and we land back here.
                let cur = addr.lock().unwrap().clone();
                match TcpStream::connect(&cur) {
                    Ok(rd) => {
                        rd.set_nodelay(true).ok();
                        // Detect a *silently* dead link (Wi-Fi/route/NAT change, suspend→resume,
                        // or just an idle desktop sending no frames) within ~20 s — otherwise the
                        // blocking read_exact below parks forever and we never reconnect.
                        if let Err(e) = wire::net::set_keepalive(&rd) {
                            tracing::warn!("keepalive setup failed: {e}");
                        }
                        if let Ok(w) = rd.try_clone() {
                            *writer.lock().unwrap() = Some(w);
                        }
                        tracing::info!("connected to {cur}");
                        // Buffer the read half: one recv fills the buffer so the per-frame
                        // tag/header/AU `read_exact`s are served from memory instead of one
                        // syscall each (the write half is the independent `try_clone` above).
                        let mut rd = std::io::BufReader::new(rd);
                        let mut tag = [0u8; 1];
                        while rd.read_exact(&mut tag).is_ok() {
                            // tags 1 (clipboard), 2 (cursor), 3 (layout), 4 (mode), 5 (forwards) are all [u32 len][json].
                            if matches!(tag[0], 1 | 2 | 3 | 4 | 5) {
                                let mut lb = [0u8; 4];
                                if rd.read_exact(&mut lb).is_err() {
                                    break;
                                }
                                let mut body = vec![0u8; u32::from_be_bytes(lb) as usize];
                                if rd.read_exact(&mut body).is_err() {
                                    break;
                                }
                                if tag[0] == 4 {
                                    // Mode handshake: arrives before the first AU; record it so
                                    // make_decoder builds the right pipeline per monitor.
                                    if let Ok(m) = serde_json::from_slice::<ModeMsg>(&body) {
                                        let v = matches!(m.chroma, ChromaMode::Yuv444) as u8;
                                        VIEWER_CHROMA.store(v, std::sync::atomic::Ordering::Relaxed);
                                        tracing::info!("server chroma mode: {:?}", m.chroma);
                                    }
                                } else if tag[0] == 1 {
                                    if let Ok(msg) = serde_json::from_slice::<ClipboardMsg>(&body) {
                                        inbox.lock().unwrap().push_back(msg);
                                    }
                                } else if tag[0] == 3 {
                                    if let Ok(l) = serde_json::from_slice::<Vec<MonitorPlacement>>(&body) {
                                        *reported.lock().unwrap() = l;
                                    }
                                } else if tag[0] == 5 {
                                    // Desired forward set: reconcile local listeners. The
                                    // data port lives on the same host as the video port.
                                    if let Ok(m) = serde_json::from_slice::<ForwardsMsg>(&body) {
                                        let server = addr.lock().unwrap().clone();
                                        let host = server
                                            .rsplit_once(':')
                                            .map(|(h, _)| h.to_string())
                                            .unwrap_or(server);
                                        let forward_addr = format!("{host}:{}", m.forward_port);
                                        fwd_mgr.reconcile(m.rules, forward_addr);
                                    }
                                } else if let Ok(c) = serde_json::from_slice::<CursorMeta>(&body) {
                                    let now = Instant::now();
                                    if c.warp {
                                        // Agent-driven move: draw the synthetic cursor on this
                                        // monitor (below) and hold off local motion sends for
                                        // WARP_SUPPRESS (both debounced — refreshed by each warp).
                                        *warp.lock().unwrap() = Some(now + WARP_SUPPRESS);
                                    }
                                    let mut map = cursors.lock().unwrap();
                                    let e = map.entry(c.monitor_id).or_default();
                                    e.x = c.x;
                                    e.y = c.y;
                                    if c.warp {
                                        e.warp_until = Some(now + AGENT_CURSOR_SHOW);
                                    }
                                    if let Some(shape) = c.shape {
                                        e.version += 1;
                                        tracing::debug!(
                                            "cursor meta: mon={} pos=({},{}) warp={} shape {}x{} hot=({},{}) → version {}",
                                            c.monitor_id, c.x, c.y, c.warp,
                                            shape.width, shape.height, shape.hotspot_x, shape.hotspot_y, e.version
                                        );
                                        e.shape = Some(shape);
                                    } else {
                                        tracing::trace!(
                                            "cursor meta: mon={} pos=({},{}) warp={} (position only)",
                                            c.monitor_id, c.x, c.y, c.warp
                                        );
                                    }
                                }
                                continue;
                            }
                            let mut hdr = [0u8; 8];
                            if rd.read_exact(&mut hdr).is_err() {
                                break;
                            }
                            let mid = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
                            let len = u32::from_be_bytes(hdr[4..8].try_into().unwrap()) as usize;
                            let mut au = vec![0u8; len];
                            if rd.read_exact(&mut au).is_err() {
                                break;
                            }
                            // Fast path: once a monitor's window (hence appsrc) exists, push the
                            // AU straight to its decoder from here — skipping the GTK tick, which
                            // otherwise cost up to one 8 ms tick of latency per frame. A monitor's
                            // first AU(s) still go via the queue for the tick to build the window on
                            // the main thread. Hold `srcs` across this dispatch (and the tick holds
                            // it across create+drain) so the hand-off stays strictly ordered — an
                            // out-of-order AU would corrupt H.264 decode.
                            let g = srcs.lock().unwrap();
                            if let Some(src) = g.get(&mid) {
                                let _ = src.push_buffer(gst::Buffer::from_mut_slice(au));
                            } else {
                                let mut q = aus.lock().unwrap();
                                if q.len() >= AU_QUEUE_CAP {
                                    q.pop_front();
                                }
                                q.push_back((mid, au));
                            }
                        }
                        *writer.lock().unwrap() = None;
                        tracing::info!("disconnected; retrying (server force-IDRs on reconnect)");
                    }
                    Err(e) => tracing::warn!("connect {cur} failed: {e}"),
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    let app = gtk4::Application::builder().application_id("dev.rmng.viewer").build();
    app.connect_activate(move |app| build_ui(app, &aus, &srcs, &writer, &inbox, &cursors, &reported, &warp, &addr));
    let empty: [&str; 0] = [];
    app.run_with_args(&empty);
    Ok(())
}

/// Per-window held-input state (one monitor's window). Held keys/buttons are released on
/// that window's focus loss; `inside` drives its shortcut grab (which follows the mouse).
#[derive(Default)]
struct WinInput {
    pressed: RefCell<HashSet<u32>>,
    buttons: RefCell<HashSet<i32>>,
    inside: Cell<bool>,
}

/// One monitor's window state we touch on every tick: the video `Picture` (sink
/// paintable), the client-drawn cursor overlay, the appsrc fed AUs, and the last cursor
/// version applied. The held `WinInput` is kept alive by the input closures, so it isn't
/// stored here.
struct MonitorWindow {
    video: gtk4::Picture,
    cursor: gtk4::Picture,
    appsrc: AppSrc,
    paintable: gdk::Paintable,
    last_version: u64,
    /// Native OS cursor built from the latest remote `CursorShape` (set on `video` so the
    /// operator's own pointer takes the remote shape — I-beam, hand, resize, …).
    native_cursor: Option<gdk::Cursor>,
    /// Whether `video`'s cursor is currently hidden (pointer-lock / relative mode).
    cursor_hidden: bool,
    /// The window itself, so a reconfigure can destroy it when its monitor is removed.
    window: gtk4::ApplicationWindow,
    /// The decode pipeline, stopped (not leaked) on window teardown.
    pipeline: gst::Pipeline,
}

type Windows = Rc<RefCell<HashMap<u32, MonitorWindow>>>;

#[allow(clippy::too_many_arguments)]
fn build_ui(
    app: &gtk4::Application,
    aus: &VideoAus,
    srcs: &VideoSrcs,
    writer: &Writer,
    inbox: &ClipInbox,
    cursors: &Cursors,
    reported: &ReportedLayout,
    warp: &WarpSuppress,
    addr: &ServerAddr,
) {
    // Black background behind every letterboxed video (applies to all windows).
    // Pointer-lock (games): one instance per display, shared across monitor windows;
    // None on X11 / when the compositor lacks the protocols / RMNG_NO_POINTER_LOCK.
    let css = gtk4::CssProvider::new();
    // Title-bar styling copied from the gtk-kasmvnc-client header bar.
    css.load_from_string(
        r#"
        /* Black background behind the letterboxed video. Scoped to the monitor
           windows (.video-window) so it does NOT paint dialogs (e.g. the Settings
           dialog, a plain window) black, which hid their text/buttons. */
        window.video-window { background: black; }
        /* The video grabs keyboard focus on hover/click; don't draw a focus ring on it. */
        picture:focus, picture:focus-visible { outline: none; }
        /* FPS readout in the title bar: tabular figures so the width does not
           jitter as the number changes, and dimmed so it stays unobtrusive. */
        .fps-readout {
            font-feature-settings: "tnum";
            opacity: 0.6;
        }
        /* The theme draws a rounded-square hover background on the
           minimize/maximize/close *buttons*, on top of the circular
           background it keeps on the icon. Suppress the square and put the
           hover feedback on the circle instead. */
        windowcontrols > button:hover,
        windowcontrols > button:active {
            background: none;
            box-shadow: none;
        }
        windowcontrols > button:hover > image {
            background-color: alpha(currentColor, 0.14);
        }
        windowcontrols > button:active > image {
            background-color: alpha(currentColor, 0.22);
        }
        "#,
    );
    let mut pointer_lock: Option<Rc<PointerLock>> = None;
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(&display, &css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
        install_clipboard(&display.clipboard(), writer, inbox);
        pointer_lock = PointerLock::new(&display, writer.clone()).map(Rc::new);
    }

    // macOS: forward PHYSICAL keys from a raw NSEvent monitor instead of GTK's key events.
    // GDK's macOS backend runs keys through the Cocoa text-input machinery and synthesizes
    // keycode-0 "null key" events for IME-committed text, which our kVK table mistranslated
    // to a phantom, out-of-order KEY_A. One app-global monitor (single remote keyboard).
    #[cfg(target_os = "macos")]
    keyboard_macos::install(writer.clone());

    // A window exists from launch, before any connection: monitor windows are built
    // lazily on each monitor's first video AU, so a wrong/unset server address used
    // to mean no window at all — and with it no Settings button to fix the address.
    // This startup window carries that button (plus a connection status) until the
    // first monitor window appears, and doubles as the app's keep-alive (GTK quits
    // an app with zero windows, which an explicit `app.hold()` used to prevent);
    // closing it while it's still the only window quits the viewer, as expected.
    let startup: Rc<RefCell<Option<gtk4::ApplicationWindow>>> =
        Rc::new(RefCell::new(Some(make_startup_window(app, addr, writer))));

    let windows: Windows = Rc::new(RefCell::new(HashMap::new()));
    let layout: SharedLayout = Rc::new(RefCell::new(Vec::new()));

    {
        let (aus, srcs, writer, cursors, windows, layout, app, reported, warp, pointer_lock, startup, addr) = (
            aus.clone(),
            srcs.clone(),
            writer.clone(),
            cursors.clone(),
            windows.clone(),
            layout.clone(),
            app.clone(),
            reported.clone(),
            warp.clone(),
            pointer_lock.clone(),
            startup.clone(),
            addr.clone(),
        );
        // ~8 ms tick: drain AUs → window per monitor (created lazily here, on the main
        // thread); refresh the layout; update client cursors.
        glib::timeout_add_local(Duration::from_millis(8), move || {
            // Bootstrap-only video path: the net thread pushes AUs for already-built monitors
            // straight to their appsrc; here we drain the first AU(s) of a not-yet-built monitor,
            // create its window on the main thread, and register its appsrc in `srcs` so the net
            // thread takes over. Hold `srcs` across the whole drain+create so the hand-off stays
            // ordered with the net thread (see its matching comment).
            {
                let mut srcs = srcs.lock().unwrap();
                let batch: Vec<(u32, Vec<u8>)> = aus.lock().unwrap().drain(..).collect();
                for (mid, au) in batch {
                    let mut w = windows.borrow_mut();
                    // The "main" window (close button + Settings; closing it quits the whole
                    // viewer) is the window for monitor_id 0. monitor_id is the daemon layout's
                    // slot index (0..n-1), and every layout has ≥1 monitor, so id 0 is present
                    // in EVERY layout — Task 4.2's reconcile (below) can never destroy it. That
                    // makes this a stable, frozen-at-build-time designation: unlike the old
                    // "primary" flag (which lived on a monitor that a live layout switch could
                    // remove, stranding the operator without a close/Settings control), window 0
                    // is main for its entire lifetime and never needs to be re-decided.
                    let is_main = mid == 0;
                    let win = w.entry(mid).or_insert_with(|| {
                        let win = make_monitor_window(&app, mid, &layout, &writer, &addr, &pointer_lock, &warp, is_main);
                        srcs.insert(mid, win.appsrc.clone());
                        // First monitor window exists: the pre-connection startup window
                        // has served its purpose. `destroy` (not `close`) skips the
                        // close-request path, and the monitor window built just above
                        // keeps the app alive.
                        if let Some(s) = startup.borrow_mut().take() {
                            s.destroy();
                        }
                        win
                    });
                    let _ = win.appsrc.push_buffer(gst::Buffer::from_mut_slice(au));
                }
            }
            // Prefer the server's reported layout (the clone's real monitor positions);
            // until it arrives, fall back to a computed left-to-right packing.
            {
                let rep = reported.lock().unwrap();
                if !rep.is_empty() {
                    *layout.borrow_mut() = rep
                        .iter()
                        .map(|m| Screen { id: m.id, x: m.x, y: m.y, w: m.width, h: m.height })
                        .collect();
                } else {
                    let mut mons: Vec<(u32, u32, u32)> = windows
                        .borrow()
                        .iter()
                        .map(|(mid, win)| {
                            let (fw, fh) = frame_size(&win.paintable);
                            (*mid, fw as u32, fh as u32)
                        })
                        .collect();
                    *layout.borrow_mut() = compute_layout(&mut mons);
                }
            }
            // Reconcile windows against the reported layout: destroy any monitor window
            // whose id vanished (a preset with fewer monitors). New ids are still built
            // lazily on their first AU; resized ids keep their window (the decoder
            // renegotiates from the new SPS). Runs on the GTK main thread.
            {
                let rep = reported.lock().unwrap();
                if !rep.is_empty() {
                    let live: std::collections::HashSet<u32> = rep.iter().map(|m| m.id).collect();
                    let mut w = windows.borrow_mut();
                    let mut srcs = srcs.lock().unwrap();
                    let gone: Vec<u32> = w.keys().copied().filter(|id| !live.contains(id)).collect();
                    for id in gone {
                        // The main window (monitor_id 0) is never in `gone`: every reported
                        // layout has ≥1 monitor at slot 0, so `live` always contains 0 — this
                        // loop structurally can't destroy it (see is_main above). The len<=1
                        // check below guards a separate, rarer race: a non-main window that
                        // happens to be the *only* live window right now (id 0's window not
                        // yet lazily built) getting destroyed here, which would leave zero
                        // windows and let GTK quit the app before id 0's first AU arrives.
                        if w.len() <= 1 {
                            break;
                        }
                        if let Some(win) = w.remove(&id) {
                            let _ = win.pipeline.set_state(gst::State::Null);
                            win.window.destroy();
                            srcs.remove(&id);
                        }
                    }
                }
            }
            // Cursor: (1) the native OS cursor over the video takes the remote's shape
            // (rebuilt from CursorShape on change), hidden only in pointer-lock; (2) the
            // synthetic overlay is drawn on top ONLY while the remote agent drives the
            // pointer (this monitor's warp window), so the operator sees the agent's target.
            let locked = pointer_lock.as_ref().is_some_and(|p| p.is_engaged());
            let now = Instant::now();
            let csnap: HashMap<u32, CursorEntry> = cursors.lock().unwrap().clone();
            for (mid, win) in windows.borrow_mut().iter_mut() {
                let entry = csnap.get(mid);
                // Rebuild the cursor texture + native gdk cursor when the remote shape changes.
                if let Some(e) = entry {
                    if e.version != win.last_version {
                        win.last_version = e.version;
                        if let Some(shape) = &e.shape {
                            if let Some(tex) = cursor_texture(shape) {
                                win.cursor.set_paintable(Some(&tex)); // overlay texture
                                let fallback = gdk::Cursor::from_name("default", None);
                                win.native_cursor = Some(gdk::Cursor::from_texture(
                                    &tex,
                                    shape.hotspot_x as i32,
                                    shape.hotspot_y as i32,
                                    fallback.as_ref(),
                                ));
                                if !locked {
                                    win.video.set_cursor(win.native_cursor.as_ref());
                                }
                                tracing::debug!(
                                    "cursor apply: mon={mid} version={} {}x{} locked={locked}{}",
                                    e.version, shape.width, shape.height,
                                    if locked { " (set_cursor SKIPPED: pointer-lock)" } else { "" }
                                );
                            } else {
                                tracing::warn!(
                                    "cursor apply: mon={mid} version={} texture build FAILED ({}x{}, {} bytes)",
                                    e.version, shape.width, shape.height, shape.rgba.len()
                                );
                            }
                        }
                    }
                }
                // Native cursor: hide for pointer-lock, else show the remote-shaped cursor.
                if locked != win.cursor_hidden {
                    tracing::debug!("cursor hide flip: mon={mid} locked={locked}");
                    if locked {
                        win.video.set_cursor_from_name(Some("none"));
                    } else {
                        win.video.set_cursor(win.native_cursor.as_ref());
                    }
                    win.cursor_hidden = locked;
                }
                // Overlay: only while the agent is driving this monitor's pointer.
                let show = !locked && entry.is_some_and(|e| e.warp_until.is_some_and(|d| now < d));
                if !show {
                    win.cursor.set_visible(false);
                    continue;
                }
                let e = entry.unwrap();
                let (scale, off_x, off_y) = letterbox(&win.video, &win.paintable);
                if let Some(shape) = &e.shape {
                    win.cursor.set_size_request(
                        (shape.width as f64 * scale).round() as i32,
                        (shape.height as f64 * scale).round() as i32,
                    );
                }
                let (hx, hy) = e.shape.as_ref().map(|s| (s.hotspot_x as i32, s.hotspot_y as i32)).unwrap_or((0, 0));
                win.cursor.set_margin_start((off_x + (e.x - hx) as f64 * scale).round().max(0.0) as i32);
                win.cursor.set_margin_top((off_y + (e.y - hy) as f64 * scale).round().max(0.0) as i32);
                win.cursor.set_visible(win.cursor.paintable().is_some());
            }
            glib::ControlFlow::Continue
        });
    }
}

/// Build one monitor's window: decode pipeline + video/cursor overlay + input controllers.
#[allow(clippy::too_many_arguments)]
fn make_monitor_window(
    app: &gtk4::Application,
    mid: u32,
    layout: &SharedLayout,
    writer: &Writer,
    addr: &ServerAddr,
    pointer_lock: &Option<Rc<PointerLock>>,
    warp: &WarpSuppress,
    is_main: bool,
) -> MonitorWindow {
    let (appsrc, paintable, pipeline) = make_decoder(mid).expect("build decoder");

    let video = gtk4::Picture::for_paintable(&paintable);
    video.set_can_shrink(true);
    video.set_content_fit(gtk4::ContentFit::Contain); // letterbox: uniform scale, black bars
    video.set_hexpand(true);
    video.set_vexpand(true);
    video.set_halign(gtk4::Align::Fill);
    video.set_valign(gtk4::Align::Fill);
    video.set_size_request(480, 270);
    // Make the video able to hold keyboard focus, and grab it on hover/click (see
    // install_pointer). Otherwise focus stays on a title-bar button (Settings/fullscreen),
    // and pressing Enter activates that button instead of reaching the remote — the
    // window-level key controller only sees keys that bubble past the focused widget.
    video.set_focusable(true);
    // Native OS cursor is shown over the video by default; the synthetic overlay below is
    // drawn only while the remote agent drives the pointer. Pointer-lock hides the native
    // cursor at its engage site (relative-motion / game mode).

    let cursor = gtk4::Picture::new();
    cursor.set_can_shrink(true);
    cursor.set_content_fit(gtk4::ContentFit::Fill);
    cursor.set_halign(gtk4::Align::Start);
    cursor.set_valign(gtk4::Align::Start);
    cursor.set_can_target(false); // input-transparent
    cursor.set_visible(false);

    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(format!("RMNG viewer — monitor {mid}"))
        .default_width(1280)
        .default_height(720)
        // Only the main window (monitor 0) gets a close button; secondary monitor
        // windows can't be closed individually (their layout mirrors the remote desktop).
        .deletable(is_main)
        .build();
    // Tag this as a monitor window so the `window.video-window { background: black }`
    // rule paints its letterbox bars black, without affecting dialogs (Settings).
    window.add_css_class("video-window");
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&video));
    overlay.add_overlay(&cursor);
    window.set_child(Some(&overlay));

    // ── Title bar ──────────────────────────────────────────────────────────────────────
    // On Linux (and non-macOS): build the GTK HeaderBar with FPS readout, fullscreen
    // button, and (main window only) server-address button.
    // On macOS: skip the GTK HeaderBar entirely; native_titlebar::install wires native
    // NSButton accessories to the real NSWindow titlebar (see native_titlebar.rs).
    // Do NOT call window.set_titlebar(...) on macOS — that path stays Linux-only.
    #[cfg(not(target_os = "macos"))]
    {
        // Header bar: FPS readout (left) + a fullscreen toggle (F11 also toggles).
        // Styling matches the gtk-kasmvnc-client title bar (see the CSS in build_ui).
        let header = gtk4::HeaderBar::new();
        // FPS readout at the top-left of the title bar.
        let fps_label = gtk4::Label::new(Some("0 FPS"));
        fps_label.add_css_class("fps-readout");
        header.pack_start(&fps_label);
        let fs_btn = gtk4::Button::from_icon_name("view-fullscreen-symbolic");
        fs_btn.set_tooltip_text(Some("Toggle fullscreen (F11)"));
        {
            let win = window.clone();
            fs_btn.connect_clicked(move |_| toggle_fullscreen(&win));
        }
        header.pack_end(&fs_btn);
        // Settings (server address) lives only on the main window's title bar, like the
        // gtk-kasmvnc-client header. pack_end after the fullscreen button puts it to its left.
        if is_main {
            let settings = gtk4::Button::from_icon_name("network-server-symbolic");
            settings.set_tooltip_text(Some("Server address"));
            let (win, addr, writer) = (window.clone(), addr.clone(), writer.clone());
            settings.connect_clicked(move |_| show_server_addr_dialog(&win, &addr, &writer));
            header.pack_end(&settings);
        }
        window.set_titlebar(Some(&header));

        // FPS: count presented frames off the paintable, report once a second.
        let present_count = Rc::new(Cell::new(0u32));
        {
            let c = present_count.clone();
            paintable.connect_invalidate_contents(move |_| c.set(c.get() + 1));
        }
        {
            let (c, label) = (present_count.clone(), fps_label.clone());
            glib::timeout_add_seconds_local(1, move || {
                label.set_text(&format!("{} FPS", c.replace(0)));
                glib::ControlFlow::Continue
            });
        }
    }
    // TODO(spike): native FPS label on macOS — add NSTextField updated from a 1s glib timer.

    // Close logic copied from gtk-kasmvnc-client: only the main window is closable,
    // and closing it quits the whole viewer (every monitor window). Secondary windows
    // have no close button (deletable=false above); block any close request that still
    // reaches them (e.g. a window-manager-initiated close).
    {
        let app = app.clone();
        window.connect_close_request(move |_| {
            if is_main {
                app.quit();
                glib::Propagation::Proceed
            } else {
                glib::Propagation::Stop
            }
        });
    }

    let state = Rc::new(WinInput::default());
    install_pointer(&video, mid, &paintable, &window, layout, writer, &state, pointer_lock, warp);
    install_keyboard(&window, writer, &state, pointer_lock);

    // macOS: register the native titlebar BEFORE present() so connect_realize fires once
    // the surface is actually ready. The closure runs asynchronously on the main thread.
    #[cfg(target_os = "macos")]
    native_titlebar::install(&window, is_main, addr, writer);

    window.present();

    MonitorWindow {
        video,
        cursor,
        appsrc,
        paintable,
        last_version: 0,
        native_cursor: None,
        cursor_hidden: false,
        window: window.clone(),
        pipeline,
    }
}

/// The pre-connection window, shown from launch until the first monitor window
/// exists (then destroyed by the tick in [`build_ui`]). Its point is the Settings
/// button: monitor windows only appear once video flows, so with a wrong server
/// address this is the only place left to fix it. Shows live connection status.
fn make_startup_window(app: &gtk4::Application, addr: &ServerAddr, writer: &Writer) -> gtk4::ApplicationWindow {
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("RMNG viewer")
        .default_width(480)
        .default_height(270)
        .build();

    // Same Settings button as the main monitor window's title bar.
    // On Linux: use the GTK HeaderBar. On macOS: use native NSButton via native_titlebar.
    #[cfg(not(target_os = "macos"))]
    {
        let header = gtk4::HeaderBar::new();
        let settings = gtk4::Button::from_icon_name("network-server-symbolic");
        settings.set_tooltip_text(Some("Server address"));
        {
            let (win, addr, writer) = (window.clone(), addr.clone(), writer.clone());
            settings.connect_clicked(move |_| show_server_addr_dialog(&win, &addr, &writer));
        }
        header.pack_end(&settings);
        window.set_titlebar(Some(&header));
    }
    // macOS: native settings button is installed after realize (see below).

    let spinner = gtk4::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_size_request(24, 24);
    let status = gtk4::Label::new(Some(&format!("Connecting to {}…", addr.lock().unwrap())));
    let hint = gtk4::Label::new(Some("Change the server address with the title-bar button."));
    hint.add_css_class("dim-label");

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    content.set_valign(gtk4::Align::Center);
    content.set_halign(gtk4::Align::Center);
    content.append(&spinner);
    content.append(&status);
    content.append(&hint);
    window.set_child(Some(&content));

    // Keep the status live: the address changes via Settings and the net thread
    // connects/retries underneath us. Ends itself once the window is gone.
    {
        let weak = window.downgrade();
        let (addr, writer) = (addr.clone(), writer.clone());
        glib::timeout_add_local(Duration::from_millis(500), move || {
            if weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            let text = if writer.lock().unwrap().is_some() {
                "Connected — waiting for video…".to_string()
            } else {
                format!("Connecting to {}…", addr.lock().unwrap())
            };
            if status.text() != text {
                status.set_text(&text);
            }
            glib::ControlFlow::Continue
        });
    }

    // macOS: install native titlebar with settings button before presenting.
    #[cfg(target_os = "macos")]
    native_titlebar::install(&window, true, addr, writer);

    window.present();
    window
}

/// Settings dialog (main window only): edit the server `host:port` and persist it to
/// the config file. On save, the net thread's current connection is dropped so it
/// reconnects to the new address. Mirrors gtk-kasmvnc-client's control-server dialog.
pub(crate) fn show_server_addr_dialog(parent: &gtk4::ApplicationWindow, addr: &ServerAddr, writer: &Writer) {
    let dialog = gtk4::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Server address")
        .default_width(420)
        .build();

    let entry = gtk4::Entry::new();
    entry.set_text(&addr.lock().unwrap().clone());
    entry.set_hexpand(true);

    let save = gtk4::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let cancel = gtk4::Button::with_label("Cancel");

    let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    buttons.set_halign(gtk4::Align::End);
    buttons.append(&cancel);
    buttons.append(&save);

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.append(&gtk4::Label::new(Some("Server address (host:port):")));
    content.append(&entry);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    // Save: validate, persist, repoint the net thread, close. Shared by the Save
    // button and pressing Enter in the entry.
    let apply = {
        let (dialog, addr, writer, entry) = (dialog.clone(), addr.clone(), writer.clone(), entry.clone());
        move || {
            let text = entry.text().trim().to_string();
            if !valid_addr(&text) {
                entry.add_css_class("error");
                return;
            }
            entry.remove_css_class("error");
            *addr.lock().unwrap() = text.clone();
            if let Err(e) = config::save(&config::Config { server_addr: text }) {
                tracing::warn!("config save failed: {e}");
            }
            // Drop the current connection so the net thread's blocking read returns; it
            // then loops, re-reads the shared address, and connects to the new server.
            if let Some(s) = writer.lock().unwrap().as_ref() {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            dialog.close();
        }
    };
    {
        let apply = apply.clone();
        save.connect_clicked(move |_| apply());
    }
    entry.connect_activate(move |_| apply());

    dialog.present();
}

/// Light `host:port` validation: a non-empty host and a port that parses as `u16`.
fn valid_addr(s: &str) -> bool {
    match s.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok(),
        None => false,
    }
}

/// Toggle a window between fullscreen and normal (F11 / header button).
pub(crate) fn toggle_fullscreen(window: &gtk4::ApplicationWindow) {
    if window.is_fullscreen() {
        window.unfullscreen();
    } else {
        window.fullscreen();
    }
}

/// One monitor's decode pipeline → `gtk4paintablesink`. Returns the appsrc + the sink's
/// `GdkPaintable`. Zero-copy GL path (works on Intel, where GStreamer can't export a VA
/// dmabuf): `vah264dec ! glupload` (EGL dmabuf→GL, shares GTK's GL context) → the sink.
fn make_decoder(monitor_id: u32) -> Result<(AppSrc, gdk::Paintable, gst::Pipeline)> {
    if VIEWER_CHROMA.load(std::sync::atomic::Ordering::Relaxed) == 1 {
        return make_decoder_yuv444(monitor_id);
    }
    // `sync=false`: present each frame on arrival rather than holding it to its clock PTS —
    // lowest latency for a live, latest-wins paintable (no audio to sync to). It also makes
    // the sink immune to any reorder/DPB latency the decoder declares in a LATENCY query, so
    // the only display delay left is the next vsync. Matches the 444 path (make_decoder_yuv444).
    //
    // macOS: vtdec_hw emits NV12 GLMemory with texture-target=rectangle (IOSurface/CGL); the sink
    // only accepts RGBA/RGB 2D GLMemory, so glcolorconvert converts rectangle→2D + NV12→RGBA in
    // one GPU pass. glupload drops out (vtdec_hw is its own GL producer). Linux string unchanged.
    #[cfg(not(target_os = "macos"))]
    let desc = "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! vah264dec ! glupload ! gtk4paintablesink name=sink sync=false";
    #[cfg(target_os = "macos")]
    let desc = "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! vtdec_hw ! glcolorconvert ! gtk4paintablesink name=sink sync=false";
    let pipeline = gst::parse::launch(desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow!("not a pipeline"))?;
    if let Some(bus) = pipeline.bus() {
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(e) => tracing::error!(
                    "decode[mon{monitor_id}] error from {:?}: {} (debug: {:?})",
                    e.src().map(|s| s.name()),
                    e.error(),
                    e.debug()
                ),
                gst::MessageView::Warning(w) => {
                    tracing::warn!("decode[mon{monitor_id}] warning: {} (debug: {:?})", w.error(), w.debug())
                }
                _ => {}
            }
            gst::BusSyncReply::Pass
        });
    }
    let appsrc = pipeline.by_name("src").context("appsrc")?.downcast::<AppSrc>().map_err(|_| anyhow!("not appsrc"))?;
    appsrc.set_caps(Some(
        &gst::Caps::builder("video/x-h264").field("stream-format", "byte-stream").field("alignment", "au").build(),
    ));
    let sink = pipeline.by_name("sink").context("gtk4paintablesink")?;
    let paintable = sink.property::<gdk::Paintable>("paintable");
    pipeline.set_state(gst::State::Playing)?;
    Ok((appsrc, paintable, pipeline))
}

/// AVC444 (`ChromaMode::Yuv444`) decode: the stream is a double-height `W×2H` NV12 carrying the
/// main view over an auxiliary chroma view. **All-GL zero-copy** path — the whole reconstruction
/// stays in VRAM (no host copies), mirroring the 4:2:0 path's structure:
///
/// `appsrc(h264) ! h264parse ! vah264dec ! glupload ! rmngavc444unpack ! gtk4paintablesink`
///
/// `vah264dec ! glupload` gives the decoded `W×2H` NV12 as GLMemory (2 textures: Y R8, UV RG8);
/// our [`glunpack`] element gathers the polyphase chroma quadrants back into `W×H` 4:4:4 and does
/// the BT.601-limited YCbCr→RGB in a single FBO render (the GPU twin of
/// [`wire::avc444::unpack_stacked_nv12_to_rgba`]); `gtk4paintablesink` shows the `W×H` RGBA
/// texture zero-copy. The returned appsrc/paintable match the 4:2:0 path's interface (intrinsic
/// size `W×H`), so the rest of the viewer (letterbox, cursor overlay, fps) is unchanged.
///
/// Do **not** put a `glcolorconvert`/`videoconvert` between the decoder (`glupload` on Linux,
/// `vtdec_hw` on macOS) and `rmngavc444unpack`: that would 4:2:0-upsample the packed chroma and
/// destroy the auxiliary view. The element reads the raw Y/UV textures.
fn make_decoder_yuv444(monitor_id: u32) -> Result<(AppSrc, gdk::Paintable, gst::Pipeline)> {
    glunpack::register()?;
    // Plain `gtk4paintablesink sync=false` (present on arrival, no audio to clock-sync to) — same as
    // the 4:2:0 path. The "old frame from a few back when downscaling" bug was NOT a sink backlog
    // (the sink is latest-wins); it was GTK's `ngl`/`vulkan` GSK renderer caching a recycled
    // GdkTexture — fixed by pinning `GSK_RENDERER=gl` in main().
    // macOS: vtdec_hw replaces vah264dec + glupload (vtdec_hw is its own GL producer, outputs
    // NV12 rectangle GLMemory). Do NOT insert glcolorconvert here: rmngavc444unpack reads the raw
    // Y/UV textures; a prior colorconvert would 4:2:0-upsample the packed auxiliary chroma and
    // destroy the AVC444 reconstruction. Rectangle→2D conversion is Task 3. Linux string unchanged.
    #[cfg(not(target_os = "macos"))]
    let desc = "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! vah264dec ! glupload ! rmngavc444unpack ! gtk4paintablesink name=sink sync=false";
    #[cfg(target_os = "macos")]
    let desc = "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! vtdec_hw ! rmngavc444unpack ! gtk4paintablesink name=sink sync=false";
    let pipeline = gst::parse::launch(desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow!("not a pipeline"))?;
    if let Some(bus) = pipeline.bus() {
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(e) => tracing::error!(
                    "decode444[mon{monitor_id}] error from {:?}: {} (debug: {:?})",
                    e.src().map(|s| s.name()),
                    e.error(),
                    e.debug()
                ),
                gst::MessageView::Warning(w) => {
                    tracing::warn!("decode444[mon{monitor_id}] warning: {} (debug: {:?})", w.error(), w.debug())
                }
                _ => {}
            }
            gst::BusSyncReply::Pass
        });
    }
    let appsrc: AppSrc =
        pipeline.by_name("src").context("appsrc")?.downcast().map_err(|_| anyhow!("not appsrc"))?;
    appsrc.set_caps(Some(
        &gst::Caps::builder("video/x-h264").field("stream-format", "byte-stream").field("alignment", "au").build(),
    ));
    let paintable = pipeline.by_name("sink").context("sink")?.property::<gdk::Paintable>("paintable");
    pipeline.set_state(gst::State::Playing)?;
    Ok((appsrc, paintable, pipeline))
}

/// Source resolution from the sink paintable (0 until the first frame → 1920×1080 default).
fn frame_size(paintable: &gdk::Paintable) -> (f64, f64) {
    let w = paintable.intrinsic_width();
    let h = paintable.intrinsic_height();
    if w > 0 && h > 0 { (w as f64, h as f64) } else { (1920.0, 1080.0) }
}

/// Letterbox transform for a video `Picture` at `ContentFit::Contain`: `(scale, off_x, off_y)`
/// mapping **frame → widget** coords (`wx = off_x + fx*scale`); invert for widget → frame.
fn letterbox(pic: &gtk4::Picture, paintable: &gdk::Paintable) -> (f64, f64, f64) {
    let (fw, fh) = frame_size(paintable);
    let (ww, wh) = (pic.width().max(1) as f64, pic.height().max(1) as f64);
    let scale = (ww / fw).min(wh / fh);
    (scale, (ww - fw * scale) / 2.0, (wh - fh * scale) / 2.0)
}

/// Pack monitors left-to-right by id, bottom edges aligned, origin at (0,0) — the layout
/// the virtual monitors take, used to route a drag across the local-window seam.
fn compute_layout(mons: &mut Vec<(u32, u32, u32)>) -> Vec<Screen> {
    mons.sort_by_key(|(id, _, _)| *id);
    let bottom = mons.iter().map(|(_, _, h)| *h).max().unwrap_or(0);
    let mut x = 0i32;
    let mut screens = Vec::with_capacity(mons.len());
    for (id, w, h) in mons.iter() {
        screens.push(Screen { id: *id, x, y: (bottom - h) as i32, w: *w, h: *h });
        x += *w as i32;
    }
    screens
}

/// Follow a button-drag past the origin monitor's edge into an adjacent one (ported from
/// the old `../gtk` client's `screens::route_drag`). `mx`/`my` are **unclamped** origin-local
/// coords (the implicit grab delivers overshoot past the edge); lift them into unified
/// desktop coords and find which monitor they land in. Dead space → pinned to the origin edge.
fn route_drag(layout: &[Screen], origin: u32, mx: f64, my: f64) -> Option<(u32, f64, f64)> {
    let o = layout.iter().find(|s| s.id == origin)?;
    let ux = o.x as f64 + mx;
    let uy = o.y as f64 + my;
    for s in layout {
        if ux >= s.x as f64 && ux < s.x as f64 + s.w as f64 && uy >= s.y as f64 && uy < s.y as f64 + s.h as f64 {
            return Some((s.id, ux - s.x as f64, uy - s.y as f64));
        }
    }
    let lx = mx.clamp(0.0, o.w.saturating_sub(1) as f64);
    let ly = my.clamp(0.0, o.h.saturating_sub(1) as f64);
    Some((origin, lx, ly))
}

/// Resolve a drag motion/release at this window's widget coords to a `(monitor, local)`
/// target, following the pointer across the seam. Inverts the letterbox transform
/// **without clamping** (recovering the implicit-grab overshoot) then `route_drag`s it.
fn drag_target(video: &gtk4::Picture, paintable: &gdk::Paintable, mid: u32, layout: &[Screen], x: f64, y: f64) -> Option<(u32, f64, f64)> {
    let o = layout.iter().find(|s| s.id == mid)?;
    let (ww, wh) = (video.width() as f64, video.height() as f64);
    if ww <= 0.0 || wh <= 0.0 {
        return None;
    }
    let (fw, fh) = (o.w as f64, o.h as f64);
    if fw <= 0.0 || fh <= 0.0 {
        return None;
    }
    let _ = paintable; // sizes come from the layout (kept in sync with the paintable)
    let scale = (ww / fw).min(wh / fh);
    let mx = (x - (ww - fw * scale) / 2.0) / scale;
    let my = (y - (wh - fh * scale) / 2.0) / scale;
    route_drag(layout, mid, mx, my)
}

#[allow(clippy::too_many_arguments)]
fn install_pointer(
    video: &gtk4::Picture,
    mid: u32,
    paintable: &gdk::Paintable,
    window: &gtk4::ApplicationWindow,
    layout: &SharedLayout,
    writer: &Writer,
    state: &Rc<WinInput>,
    pointer_lock: &Option<Rc<PointerLock>>,
    warp: &WarpSuppress,
) {
    let motion = gtk4::EventControllerMotion::new();
    {
        let (state, window2, video2, pl) = (state.clone(), window.clone(), video.clone(), pointer_lock.clone());
        motion.connect_enter(move |_c, x, y| {
            tracing::debug!("pointer enter: mon={mid} at ({x:.0},{y:.0})");
            state.inside.set(true);
            grab_keys(&window2);
            // Pull keyboard focus onto the video while the pointer is over it (mirrors
            // grab_keys), so keys reach the remote rather than a focused title-bar button.
            video2.grab_focus();
            // GTK 4.20 Wayland stuck-cursor workaround. When the pointer crosses a monitor
            // seam out of this window, GDK's cursor-surface scale listener re-sends
            // wl_pointer.set_cursor AFTER the leave with the stale enter serial (mutter
            // ignores it) and marks its cursor surface as attached; the next enter then
            // skips set_cursor entirely (buffer-attach + wl_surface.offset only), so the
            // compositor never binds our cursor and remote shape updates become invisible
            // no-ops until the following crossing. Bouncing through a *named* cursor takes
            // GDK's cursor-shape path (clearing the attached flag), and restoring the
            // texture cursor then forces a full set_cursor with the current enter serial.
            // Wayland-specific; gated so macOS doesn't get the unnecessary bounce.
            #[cfg(target_os = "linux")]
            if !pl.as_ref().is_some_and(|p| p.is_engaged()) {
                if let Some(cur) = video2.cursor() {
                    video2.set_cursor_from_name(Some("default"));
                    video2.set_cursor(Some(&cur));
                }
            }
            // On non-Linux `pl` is captured by the closure but used only in the Linux
            // block above; suppress the unused-variable warning without a rename.
            #[cfg(not(target_os = "linux"))]
            let _ = &pl;
        });
    }
    {
        let (state, window2) = (state.clone(), window.clone());
        motion.connect_leave(move |_c| {
            tracing::debug!("pointer leave: mon={mid}");
            state.inside.set(false);
            // Don't ungrab mid-drag: the implicit grab carries the pointer off the edge.
            if state.buttons.borrow().is_empty() {
                ungrab_shortcuts(&window2);
            }
        });
    }
    {
        let (w, state, layout, video2, paintable2, pl, warp) = (
            writer.clone(),
            state.clone(),
            layout.clone(),
            video.clone(),
            paintable.clone(),
            pointer_lock.clone(),
            warp.clone(),
        );
        motion.connect_motion(move |_c, x, y| {
            // Pointer-lock engaged (games): the relative-pointer thread sends motion; skip
            // the absolute path entirely.
            if pl.as_ref().is_some_and(|p| p.is_engaged()) {
                return;
            }
            // Just after an agent-driven warp: hold off local motion so the user's mouse
            // doesn't yank the cursor off the agent's target (debounced; see WarpSuppress).
            if warp.lock().unwrap().is_some_and(|deadline| Instant::now() < deadline) {
                return;
            }
            // Mid-drag (held button): follow the pointer across the seam so a remote
            // window-move continues onto the neighbour instead of stalling at the edge.
            let (tmid, mx, my) = if !state.buttons.borrow().is_empty() {
                match drag_target(&video2, &paintable2, mid, &layout.borrow(), x, y) {
                    Some(t) => t,
                    None => return,
                }
            } else {
                let (s, off_x, off_y) = letterbox(&video2, &paintable2);
                let (fw, fh) = frame_size(&paintable2);
                (mid, ((x - off_x) / s).clamp(0.0, fw), ((y - off_y) / s).clamp(0.0, fh))
            };
            send(&w, format!(r#"{{"kind":"pointer_move","monitor_id":{tmid},"x":{mx:.1},"y":{my:.1}}}"#));
        });
    }
    video.add_controller(motion);

    let click = gtk4::GestureClick::new();
    click.set_button(0);
    {
        let (w, state, video2) = (writer.clone(), state.clone(), video.clone());
        click.connect_pressed(move |g, _n, _x, _y| {
            // Take focus off any title-bar button so Enter/Space reach the remote (covers
            // the case where the pointer was already over the video when the window
            // activated, so no `enter` fired).
            video2.grab_focus();
            let Some(b) = evdev_button(g.current_button()) else { return };
            state.buttons.borrow_mut().insert(b);
            send(&w, format!(r#"{{"kind":"button","button":{b},"pressed":true}}"#));
        });
    }
    {
        // A release ends a possible drag: position the cursor at the resolved cross-seam
        // target (so the button-up lands where the drag actually is), then release.
        let (w, state, layout, video2, paintable2) =
            (writer.clone(), state.clone(), layout.clone(), video.clone(), paintable.clone());
        click.connect_released(move |g, _n, x, y| {
            if let Some((tmid, mx, my)) = drag_target(&video2, &paintable2, mid, &layout.borrow(), x, y) {
                send(&w, format!(r#"{{"kind":"pointer_move","monitor_id":{tmid},"x":{mx:.1},"y":{my:.1}}}"#));
            }
            let Some(b) = evdev_button(g.current_button()) else { return };
            state.buttons.borrow_mut().remove(&b);
            send(&w, format!(r#"{{"kind":"button","button":{b},"pressed":false}}"#));
        });
    }
    video.add_controller(click);

    let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::BOTH_AXES);
    {
        let w = writer.clone();
        scroll.connect_scroll(move |_c, dx, dy| {
            if dy != 0.0 {
                send(&w, format!(r#"{{"kind":"axis","axis":0,"step":{}}}"#, if dy > 0.0 { 1 } else { -1 }));
            }
            if dx != 0.0 {
                send(&w, format!(r#"{{"kind":"axis","axis":1,"step":{}}}"#, if dx > 0.0 { 1 } else { -1 }));
            }
            glib::Propagation::Proceed
        });
    }
    video.add_controller(scroll);
}

fn release_keycode(writer: &Writer, keycode: u32) {
    send(writer, format!(r#"{{"kind":"key_code","keycode":{keycode},"pressed":false}}"#));
}
fn release_button(writer: &Writer, button: i32) {
    send(writer, format!(r#"{{"kind":"button","button":{button},"pressed":false}}"#));
}

/// Ask the local Wayland compositor to forward all shortcuts (Super, Alt+Tab, …) to this
/// window — so they reach the remote and their key-release isn't eaten. GNOME keeps a
/// Super+Esc escape hatch. `RMNG_NO_GRAB=1` opts out.
fn grab_keys(window: &gtk4::ApplicationWindow) {
    if std::env::var_os("RMNG_NO_GRAB").is_some() {
        return;
    }
    if let Some(tl) = window.surface().and_downcast::<gdk::Toplevel>() {
        if !tl.is_shortcuts_inhibited() {
            tl.inhibit_system_shortcuts(None::<&gdk::Event>);
        }
    }
}

/// Hand shortcuts back (pointer left the view). Does NOT release held keys — focus is
/// retained, so real key-ups still arrive; an early release would drop a held Shift.
fn ungrab_shortcuts(window: &gtk4::ApplicationWindow) {
    if let Some(tl) = window.surface().and_downcast::<gdk::Toplevel>() {
        if tl.is_shortcuts_inhibited() {
            tl.restore_system_shortcuts();
        }
    }
}

/// Genuine focus loss (Alt+Tab, lock screen): release every key + button this window holds.
fn release_all_input(writer: &Writer, state: &WinInput) {
    for kc in state.pressed.borrow_mut().drain().collect::<Vec<_>>() {
        release_keycode(writer, kc);
    }
    for b in state.buttons.borrow_mut().drain().collect::<Vec<_>>() {
        release_button(writer, b);
    }
}

fn install_keyboard(
    window: &gtk4::ApplicationWindow,
    writer: &Writer,
    state: &Rc<WinInput>,
    pointer_lock: &Option<Rc<PointerLock>>,
) {
    let key = gtk4::EventControllerKey::new();
    {
        let (w, state, window2, pl) = (writer.clone(), state.clone(), window.clone(), pointer_lock.clone());
        key.connect_key_pressed(move |_c, keyval, code, s| {
            // Local viewer shortcuts (handled here, NOT forwarded to the remote):
            //   F11 — fullscreen · Ctrl+Alt+G — toggle pointer-lock · Ctrl+Alt+P — release
            //   pointer-lock + UNSTICK all keys (panic).
            // A shortcut's own modifiers (Ctrl/Alt) were already forwarded as presses before
            // we knew it was a shortcut; engaging pointer-lock / fullscreen is a grab/focus
            // transition that can swallow their key-up → a key stuck down on the clone. So
            // every shortcut releases all keys currently held on the remote as it fires.
            if keyval == gdk::Key::F11 {
                toggle_fullscreen(&window2);
                return glib::Propagation::Stop;
            }
            let ctrl_alt =
                s.contains(gdk::ModifierType::CONTROL_MASK) && s.contains(gdk::ModifierType::ALT_MASK);
            if ctrl_alt && (keyval == gdk::Key::g || keyval == gdk::Key::G) {
                release_all_input(&w, &state); // drop the leaked Ctrl/Alt before entering the game
                #[cfg(target_os = "macos")]
                keyboard_macos::release_all(); // keys are held in the NSEvent monitor's set, not `state`
                if let Some(pl) = &pl {
                    if pl.is_engaged() {
                        pl.release();
                    } else if let Some(surface) = window2.surface() {
                        pl.engage(&surface);
                    }
                }
                // The tick hides/restores the video cursor from `is_engaged()`.
                return glib::Propagation::Stop;
            }
            if ctrl_alt && (keyval == gdk::Key::p || keyval == gdk::Key::P) {
                // Panic / unstick: release the pointer-lock AND every key+button the remote
                // still thinks is held (use this any time a key gets stuck down).
                if let Some(pl) = &pl {
                    pl.release();
                }
                release_all_input(&w, &state);
                #[cfg(target_os = "macos")]
                keyboard_macos::release_all(); // keys are held in the NSEvent monitor's set, not `state`
                return glib::Propagation::Stop;
            }
            // Physical key identity → Linux evdev keycode sent on the wire.
            // Linux/X11: GTK hardware_keycode = evdev + 8; subtract 8 to recover evdev.
            #[cfg(not(target_os = "macos"))]
            {
                let keycode = code.saturating_sub(8);
                state.pressed.borrow_mut().insert(keycode);
                send(&w, format!(r#"{{"kind":"key_code","keycode":{keycode},"pressed":true}}"#));
            }
            // macOS: physical keys are forwarded by the raw NSEvent monitor (keyboard_macos),
            // NOT from here — GTK's key events on macOS come through the Cocoa text-input
            // machinery, which synthesizes phantom keycode-0 presses. This handler keeps only
            // the local shortcuts above; the monitor passes those keys through so they reach it.
            #[cfg(target_os = "macos")]
            let _ = code;
            glib::Propagation::Proceed
        });
    }
    {
        let (w, state) = (writer.clone(), state.clone());
        key.connect_key_released(move |_c, _keyval, code, _s| {
            // Mirror the press-side translation so pressed/released are symmetric.
            #[cfg(not(target_os = "macos"))]
            {
                let keycode = code.saturating_sub(8);
                state.pressed.borrow_mut().remove(&keycode);
                release_keycode(&w, keycode);
            }
            // macOS: releases come from the keyboard_macos NSEvent monitor (see key_pressed).
            #[cfg(target_os = "macos")]
            let _ = (&w, &state, code);
        });
    }
    window.add_controller(key);

    {
        let (w, state, window2) = (writer.clone(), state.clone(), window.clone());
        window.connect_is_active_notify(move |win| {
            tracing::debug!("window active: {:?} active={}", win.title().map(|t| t.to_string()), win.is_active());
            // macOS: gate the NSEvent keyboard monitor on whether a video window is the key
            // window — so keys reach the remote when looking at it, but stay local (dialogs,
            // the pre-connection window) otherwise.
            #[cfg(target_os = "macos")]
            keyboard_macos::note_window_active(win.is_active());
            if win.is_active() {
                if state.inside.get() {
                    grab_keys(&window2);
                }
            } else {
                ungrab_shortcuts(&window2);
                release_all_input(&w, &state);
                #[cfg(target_os = "macos")]
                keyboard_macos::release_all(); // drop remote-held keys tracked by the monitor
            }
        });
    }
}

/// Texture the cursor bitmap (SPA delivers BGRA8888 premultiplied, tightly packed).
fn cursor_texture(shape: &CursorShape) -> Option<gdk::Texture> {
    let need = (shape.width as usize) * (shape.height as usize) * 4;
    if shape.width == 0 || shape.height == 0 || shape.rgba.len() < need {
        return None;
    }
    let bytes = glib::Bytes::from(&shape.rgba[..need]);
    let tex = gdk::MemoryTexture::new(
        shape.width as i32,
        shape.height as i32,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        (shape.width * 4) as usize,
    );
    Some(tex.upcast())
}

/// Pick the MIMEs to fetch from an offer — up to one per category: best image,
/// `text/html`, best plain text. Plain text must be fetched *alongside* the rich
/// type: Chromium apps (VSCode) offer `text/html` + `text/plain` together, and a
/// local clipboard holding only html can't paste into plain-text targets.
fn pick_mimes(mimes: &[String]) -> Vec<String> {
    let image = mimes.iter().find(|m| m.starts_with("image/png"))
        .or_else(|| mimes.iter().find(|m| m.starts_with("image/")));
    let html = mimes.iter().find(|m| *m == "text/html");
    let text = mimes.iter().find(|m| m.starts_with("text/plain;charset=utf-8"))
        .or_else(|| mimes.iter().find(|m| is_text_mime(m)));
    let mut out: Vec<String> = [image, html, text].into_iter().flatten().cloned().collect();
    if out.is_empty() {
        out.extend(mimes.first().cloned()); // unknown-only offer: mirror it as-is
    }
    out
}

/// Bidirectional **rich + lazy** clipboard over the broker protocol (display-wide, shared
/// across all monitor windows). Bytes move only on paste; `applying` suppresses the echo.
fn install_clipboard(clipboard: &gdk::Clipboard, writer: &Writer, inbox: &ClipInbox) {
    let clipboard = clipboard.clone();
    let applying = Rc::new(std::cell::Cell::new(false));
    let serial = Rc::new(AtomicU64::new(1));

    {
        let (clipboard, inbox, writer, applying) =
            (clipboard.clone(), inbox.clone(), writer.clone(), applying.clone());
        // The remote offer currently being mirrored: its serial + the per-MIME bytes
        // collected so far. Rebuilt into a union provider as each Data reply lands,
        // so rich types and plain text are both pasteable locally.
        let mut cur_serial: u64 = 0;
        let mut collected: Vec<(String, Vec<u8>)> = Vec::new();
        glib::timeout_add_local(Duration::from_millis(80), move || {
            let msgs: Vec<ClipboardMsg> = inbox.lock().unwrap().drain(..).collect();
            for msg in msgs {
                match msg {
                    ClipboardMsg::Offer(o) => {
                        cur_serial = o.serial;
                        collected.clear();
                        let wanted = pick_mimes(&o.mime_types);
                        tracing::debug!(target: "clip",
                            "remote offer serial={} mimes={:?} -> requesting {wanted:?}",
                            o.serial, o.mime_types);
                        for mime_type in wanted {
                            let req = ClipboardRequest { serial: o.serial, mime_type };
                            if let Ok(json) = serde_json::to_string(&ClipboardMsg::Request(req)) {
                                send_tagged(&writer, 1, json);
                            }
                        }
                    }
                    ClipboardMsg::Data(d) => {
                        if d.serial != cur_serial {
                            tracing::debug!(target: "clip",
                                "dropping stale data serial={} mime={} (current serial {cur_serial})",
                                d.serial, d.mime_type);
                            continue;
                        }
                        if d.bytes.is_empty() {
                            tracing::warn!(target: "clip",
                                "empty data for mime={} serial={} (remote read failed?)",
                                d.mime_type, d.serial);
                            continue;
                        }
                        tracing::debug!(target: "clip",
                            "data serial={} mime={} ({} bytes)", d.serial, d.mime_type, d.bytes.len());
                        collected.retain(|(m, _)| m != &d.mime_type);
                        collected.push((d.mime_type, d.bytes));
                        let providers: Vec<gdk::ContentProvider> = collected
                            .iter()
                            .map(|(mime, bytes)| {
                                // Text goes in as a GValue string so GDK advertises the
                                // full set of text targets (what set_text does), not
                                // just the one exact MIME string.
                                if is_text_mime(mime) {
                                    if let Ok(text) = std::str::from_utf8(bytes) {
                                        return gdk::ContentProvider::for_value(&glib::Value::from(text));
                                    }
                                }
                                let bytes = glib::Bytes::from(bytes.as_slice());
                                gdk::ContentProvider::for_bytes(mime, &bytes)
                            })
                            .collect();
                        let provider = match providers.as_slice() {
                            [single] => single.clone(),
                            many => gdk::ContentProvider::new_union(many),
                        };
                        applying.set(true);
                        if let Err(e) = clipboard.set_content(Some(&provider)) {
                            applying.set(false);
                            tracing::warn!(target: "clip", "set_content failed: {e}");
                        } else {
                            tracing::debug!(target: "clip", "local clipboard set: {:?}",
                                collected.iter().map(|(m, b)| format!("{m} ({}B)", b.len())).collect::<Vec<_>>());
                        }
                    }
                    ClipboardMsg::Request(r) => serve_request(&clipboard, &writer, r),
                }
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let (writer, serial, applying) = (writer.clone(), serial.clone(), applying.clone());
        clipboard.connect_changed(move |cb| {
            if applying.replace(false) {
                return;
            }
            let mimes: Vec<String> = cb.formats().mime_types().iter().map(|s| s.to_string()).collect();
            if mimes.is_empty() {
                return;
            }
            tracing::debug!(target: "clip", "local copy: offering {mimes:?}");
            let offer = ClipboardOffer { serial: serial.fetch_add(1, Ordering::Relaxed), mime_types: mimes };
            if let Ok(json) = serde_json::to_string(&ClipboardMsg::Offer(offer)) {
                send_tagged(&writer, 1, json);
            }
        });
    }
}

/// Serve a remote `Request` by reading the local clipboard for the MIME and replying.
fn serve_request(clipboard: &gdk::Clipboard, writer: &Writer, r: ClipboardRequest) {
    let (serial, mime) = (r.serial, r.mime_type);
    tracing::debug!(target: "clip", "remote requests local clipboard: serial={serial} mime={mime}");
    let reply = {
        let writer = writer.clone();
        move |mime: String, bytes: Vec<u8>| {
            tracing::debug!(target: "clip", "serving serial={serial} mime={mime} ({} bytes)", bytes.len());
            let data = ClipboardData { serial, mime_type: mime, bytes };
            if let Ok(json) = serde_json::to_string(&ClipboardMsg::Data(data)) {
                send_tagged(&writer, 1, json);
            }
        }
    };
    if is_text_mime(&mime) {
        let reply = reply.clone();
        clipboard.read_text_async(gtk4::gio::Cancellable::NONE, move |res| {
            let bytes = match res { Ok(Some(t)) => t.to_string().into_bytes(), _ => Vec::new() };
            reply(mime, bytes);
        });
    } else {
        let mime2 = mime.clone();
        clipboard.read_async(&[mime.as_str()], glib::Priority::DEFAULT, gtk4::gio::Cancellable::NONE, move |res| {
            let Ok((stream, _)) = res else { return reply(mime2, Vec::new()) };
            let out = gtk4::gio::MemoryOutputStream::new_resizable();
            let out2 = out.clone();
            out.splice_async(
                &stream,
                gtk4::gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gtk4::gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                glib::Priority::DEFAULT,
                gtk4::gio::Cancellable::NONE,
                move |_| {
                    let bytes = out2.steal_as_bytes().to_vec();
                    reply(mime2, bytes);
                },
            );
        });
    }
}

/// viewer → server framing: `[u8 tag][u32be len][json]`. tag 0 = input, 1 = clipboard.
fn send_tagged(writer: &Writer, tag: u8, json: String) {
    // Hold one guard for the whole op: a second `writer.lock()` on the error path
    // below would self-deadlock (the guard from this `if let` is still alive).
    let mut guard = writer.lock().unwrap();
    if let Some(g) = guard.as_mut() {
        // One contiguous `[tag][u32be len][json]` write: with TCP_NODELAY, three separate
        // write_all calls can emit three tiny segments per input event, adding round-trip
        // jitter on a real link. Coalescing → one syscall, one segment.
        let body = json.as_bytes();
        let mut frame = Vec::with_capacity(1 + 4 + body.len());
        frame.push(tag);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        if g.write_all(&frame).is_err() {
            // Dead link surfaced on the write side (TCP_USER_TIMEOUT bounds this to
            // ~20 s). Shut the shared socket down so the net thread's parked read_exact
            // returns now and the reconnect loop starts immediately, instead of waiting
            // out the read-side keepalive window; then drop the write half. (The reader
            // owns a `try_clone` of the same kernel socket, so this unblocks it too —
            // the same mechanism the Settings dialog uses to repoint live.)
            let _ = g.shutdown(std::net::Shutdown::Both);
            *guard = None;
        }
    }
}

fn send(writer: &Writer, json: String) {
    send_tagged(writer, 0, json);
}

/// GTK/X button number → evdev code. 8/9 are the thumb back/forward buttons;
/// anything else unknown must NOT fall back to BTN_LEFT (a phantom left click).
fn evdev_button(n: u32) -> Option<i32> {
    match n {
        1 => Some(0x110), // BTN_LEFT
        2 => Some(0x112), // BTN_MIDDLE
        3 => Some(0x111), // BTN_RIGHT
        8 => Some(0x113), // BTN_SIDE  (back)
        9 => Some(0x114), // BTN_EXTRA (forward)
        _ => None,
    }
}
