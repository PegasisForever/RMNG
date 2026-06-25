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
//! viewer→server: `[u8 tag][u32be len][json]` (0 input, 1 clipboard). Auto-reconnects.
//!
//!   RMNG_VIDEO=host:port viewer [--headless]
//!
//! `gtk4paintablesink`'s paintable is a GTK object (`!Send`), so all pipelines, paintables
//! and widgets live on the GTK main thread; the net thread only ships AU bytes over a queue.

mod headless;
mod pointer_lock;

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
use wire::socket::{
    ClipboardData, ClipboardMsg, ClipboardOffer, ClipboardRequest, CursorMeta, CursorShape, MonitorPlacement,
};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    gst::init()?;
    if std::env::args().any(|a| a == "--headless") {
        return headless::run();
    }
    run_gui()
}

/// Inbound video AUs `(monitor_id, AnnexB)` shipped net-thread → GTK main thread.
type VideoAus = Arc<Mutex<VecDeque<(u32, Vec<u8>)>>>;
/// Cap so a stalled GTK thread can't grow the queue unboundedly (drops oldest).
const AU_QUEUE_CAP: usize = 300;
/// Input/clipboard write half (None while disconnected).
type Writer = Arc<Mutex<Option<TcpStream>>>;
/// Inbound clipboard messages, drained on the GUI thread (GTK clipboard ops run there).
type ClipInbox = Arc<Mutex<VecDeque<ClipboardMsg>>>;
fn is_text_mime(m: &str) -> bool {
    m.starts_with("text/plain") || m == "UTF8_STRING" || m == "TEXT"
}

/// Latest cursor state per monitor (client-drawn cursor). The shape persists across
/// position-only updates; `version` bumps on shape change so the GUI re-textures lazily.
#[derive(Default, Clone)]
struct CursorEntry {
    x: i32,
    y: i32,
    shape: Option<CursorShape>,
    version: u64,
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
    let addr = std::env::var("RMNG_VIDEO").unwrap_or_else(|_| "127.0.0.1:9001".into());
    let aus: VideoAus = Arc::new(Mutex::new(VecDeque::new()));
    let writer: Writer = Arc::new(Mutex::new(None));
    let inbox: ClipInbox = Arc::new(Mutex::new(VecDeque::new()));
    let cursors: Cursors = Arc::new(Mutex::new(HashMap::new()));
    let reported: ReportedLayout = Arc::new(Mutex::new(Vec::new()));
    let warp: WarpSuppress = Arc::new(Mutex::new(None));

    // Net thread: reconnect loop; read [u8 tag][…] → video queue / clipboard / cursor / layout.
    {
        let (aus, writer, inbox, cursors, reported, warp, addr) =
            (aus.clone(), writer.clone(), inbox.clone(), cursors.clone(), reported.clone(), warp.clone(), addr.clone());
        std::thread::spawn(move || {
            loop {
                match TcpStream::connect(&addr) {
                    Ok(mut rd) => {
                        rd.set_nodelay(true).ok();
                        if let Ok(w) = rd.try_clone() {
                            *writer.lock().unwrap() = Some(w);
                        }
                        tracing::info!("connected to {addr}");
                        let mut tag = [0u8; 1];
                        while rd.read_exact(&mut tag).is_ok() {
                            // tags 1 (clipboard), 2 (cursor), 3 (layout) are all [u32 len][json].
                            if tag[0] == 1 || tag[0] == 2 || tag[0] == 3 {
                                let mut lb = [0u8; 4];
                                if rd.read_exact(&mut lb).is_err() {
                                    break;
                                }
                                let mut body = vec![0u8; u32::from_be_bytes(lb) as usize];
                                if rd.read_exact(&mut body).is_err() {
                                    break;
                                }
                                if tag[0] == 1 {
                                    if let Ok(msg) = serde_json::from_slice::<ClipboardMsg>(&body) {
                                        inbox.lock().unwrap().push_back(msg);
                                    }
                                } else if tag[0] == 3 {
                                    if let Ok(l) = serde_json::from_slice::<Vec<MonitorPlacement>>(&body) {
                                        *reported.lock().unwrap() = l;
                                    }
                                } else if let Ok(c) = serde_json::from_slice::<CursorMeta>(&body) {
                                    if c.warp {
                                        // Agent-driven move: snap the drawn cursor (below) and
                                        // hold off local motion sends for WARP_SUPPRESS (debounced).
                                        *warp.lock().unwrap() = Some(Instant::now() + WARP_SUPPRESS);
                                    }
                                    let mut map = cursors.lock().unwrap();
                                    let e = map.entry(c.monitor_id).or_default();
                                    e.x = c.x;
                                    e.y = c.y;
                                    if c.shape.is_some() {
                                        e.shape = c.shape;
                                        e.version += 1;
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
                            let mut q = aus.lock().unwrap();
                            if q.len() >= AU_QUEUE_CAP {
                                q.pop_front();
                            }
                            q.push_back((mid, au));
                        }
                        *writer.lock().unwrap() = None;
                        tracing::info!("disconnected; retrying (server force-IDRs on reconnect)");
                    }
                    Err(e) => tracing::warn!("connect {addr} failed: {e}"),
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    let app = gtk4::Application::builder().application_id("dev.kasm.rmng-viewer").build();
    app.connect_activate(move |app| build_ui(app, &aus, &writer, &inbox, &cursors, &reported, &warp));
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
/// version applied. The `ApplicationWindow` itself is kept alive by the GTK application's
/// window list and the held `WinInput` by the input closures, so neither is stored here.
struct MonitorWindow {
    video: gtk4::Picture,
    cursor: gtk4::Picture,
    appsrc: AppSrc,
    paintable: gdk::Paintable,
    last_version: u64,
}

type Windows = Rc<RefCell<HashMap<u32, MonitorWindow>>>;

fn build_ui(
    app: &gtk4::Application,
    aus: &VideoAus,
    writer: &Writer,
    inbox: &ClipInbox,
    cursors: &Cursors,
    reported: &ReportedLayout,
    warp: &WarpSuppress,
) {
    // Keep the app alive even before the first monitor's window exists.
    std::mem::forget(app.hold());

    // Black background behind every letterboxed video (applies to all windows).
    // Pointer-lock (games): one instance per display, shared across monitor windows;
    // None on X11 / when the compositor lacks the protocols / RMNG_NO_POINTER_LOCK.
    let css = gtk4::CssProvider::new();
    css.load_from_string("window { background: black; }");
    let mut pointer_lock: Option<Rc<PointerLock>> = None;
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(&display, &css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
        install_clipboard(&display.clipboard(), writer, inbox);
        pointer_lock = PointerLock::new(&display, writer.clone()).map(Rc::new);
    }

    let windows: Windows = Rc::new(RefCell::new(HashMap::new()));
    let layout: SharedLayout = Rc::new(RefCell::new(Vec::new()));

    {
        let (aus, writer, cursors, windows, layout, app, reported, warp, pointer_lock) = (
            aus.clone(),
            writer.clone(),
            cursors.clone(),
            windows.clone(),
            layout.clone(),
            app.clone(),
            reported.clone(),
            warp.clone(),
            pointer_lock.clone(),
        );
        // ~8 ms tick: drain AUs → window per monitor (created lazily here, on the main
        // thread); refresh the layout; update client cursors.
        glib::timeout_add_local(Duration::from_millis(8), move || {
            let batch: Vec<(u32, Vec<u8>)> = aus.lock().unwrap().drain(..).collect();
            for (mid, au) in batch {
                let mut w = windows.borrow_mut();
                let win = w
                    .entry(mid)
                    .or_insert_with(|| make_monitor_window(&app, mid, &layout, &writer, &pointer_lock, &warp));
                let _ = win.appsrc.push_buffer(gst::Buffer::from_mut_slice(au));
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
            // Cursor: position (+ re-texture on shape change) per window.
            let csnap: Vec<(u32, CursorEntry)> =
                cursors.lock().unwrap().iter().map(|(m, e)| (*m, e.clone())).collect();
            for (mid, e) in csnap {
                let mut w = windows.borrow_mut();
                let Some(win) = w.get_mut(&mid) else { continue };
                let (scale, off_x, off_y) = letterbox(&win.video, &win.paintable);
                if e.version != win.last_version {
                    win.last_version = e.version;
                    if let Some(shape) = &e.shape {
                        if let Some(tex) = cursor_texture(shape) {
                            win.cursor.set_paintable(Some(&tex));
                        }
                    }
                }
                if let Some(shape) = &e.shape {
                    win.cursor.set_size_request(
                        (shape.width as f64 * scale).round() as i32,
                        (shape.height as f64 * scale).round() as i32,
                    );
                }
                let (hx, hy) = e.shape.as_ref().map(|s| (s.hotspot_x as i32, s.hotspot_y as i32)).unwrap_or((0, 0));
                win.cursor.set_margin_start((off_x + (e.x - hx) as f64 * scale).round().max(0.0) as i32);
                win.cursor.set_margin_top((off_y + (e.y - hy) as f64 * scale).round().max(0.0) as i32);
                if win.cursor.paintable().is_some() {
                    win.cursor.set_visible(true);
                }
            }
            glib::ControlFlow::Continue
        });
    }
}

/// Build one monitor's window: decode pipeline + video/cursor overlay + input controllers.
fn make_monitor_window(
    app: &gtk4::Application,
    mid: u32,
    layout: &SharedLayout,
    writer: &Writer,
    pointer_lock: &Option<Rc<PointerLock>>,
    warp: &WarpSuppress,
) -> MonitorWindow {
    let (appsrc, paintable) = make_decoder(mid).expect("build decoder");

    let video = gtk4::Picture::for_paintable(&paintable);
    video.set_can_shrink(true);
    video.set_content_fit(gtk4::ContentFit::Contain); // letterbox: uniform scale, black bars
    video.set_hexpand(true);
    video.set_vexpand(true);
    video.set_halign(gtk4::Align::Fill);
    video.set_valign(gtk4::Align::Fill);
    video.set_size_request(480, 270);
    video.set_cursor_from_name(Some("none")); // we render the remote cursor ourselves

    let cursor = gtk4::Picture::new();
    cursor.set_can_shrink(true);
    cursor.set_content_fit(gtk4::ContentFit::Fill);
    cursor.set_halign(gtk4::Align::Start);
    cursor.set_valign(gtk4::Align::Start);
    cursor.set_can_target(false); // input-transparent
    cursor.set_visible(false);

    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&video));
    overlay.add_overlay(&cursor);

    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(format!("RMNG viewer — monitor {mid}"))
        .default_width(1280)
        .default_height(720)
        .build();
    window.set_child(Some(&overlay));

    // Header bar: FPS readout (left) + a fullscreen toggle (F11 also toggles).
    let header = gtk4::HeaderBar::new();
    let fps_label = gtk4::Label::new(Some("— fps"));
    fps_label.add_css_class("dim-label");
    header.pack_start(&fps_label);
    let fs_btn = gtk4::Button::from_icon_name("view-fullscreen-symbolic");
    {
        let win = window.clone();
        fs_btn.connect_clicked(move |_| toggle_fullscreen(&win));
    }
    header.pack_end(&fs_btn);
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
            label.set_text(&format!("{} fps", c.replace(0)));
            glib::ControlFlow::Continue
        });
    }

    let state = Rc::new(WinInput::default());
    install_pointer(&video, mid, &paintable, &window, layout, writer, &state, pointer_lock, warp);
    install_keyboard(&window, writer, &state, pointer_lock);
    window.present();

    MonitorWindow { video, cursor, appsrc, paintable, last_version: 0 }
}

/// Toggle a window between fullscreen and normal (F11 / header button).
fn toggle_fullscreen(window: &gtk4::ApplicationWindow) {
    if window.is_fullscreen() {
        window.unfullscreen();
    } else {
        window.fullscreen();
    }
}

/// One monitor's decode pipeline → `gtk4paintablesink`. Returns the appsrc + the sink's
/// `GdkPaintable`. Zero-copy GL path (works on Intel, where GStreamer can't export a VA
/// dmabuf): `vah264dec ! glupload` (EGL dmabuf→GL, shares GTK's GL context) → the sink.
fn make_decoder(monitor_id: u32) -> Result<(AppSrc, gdk::Paintable)> {
    let desc = "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! vah264dec ! glupload ! gtk4paintablesink name=sink";
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
    std::mem::forget(pipeline);
    Ok((appsrc, paintable))
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
        let (state, window2) = (state.clone(), window.clone());
        motion.connect_enter(move |_c, _x, _y| {
            state.inside.set(true);
            grab_keys(&window2);
        });
    }
    {
        let (state, window2) = (state.clone(), window.clone());
        motion.connect_leave(move |_c| {
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
        let (w, state) = (writer.clone(), state.clone());
        click.connect_pressed(move |g, _n, _x, _y| {
            let b = evdev_button(g.current_button());
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
            let b = evdev_button(g.current_button());
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
                if let Some(pl) = &pl {
                    if pl.is_engaged() {
                        pl.release();
                    } else if let Some(surface) = window2.surface() {
                        pl.engage(&surface);
                    }
                }
                return glib::Propagation::Stop;
            }
            if ctrl_alt && (keyval == gdk::Key::p || keyval == gdk::Key::P) {
                // Panic / unstick: release the pointer-lock AND every key+button the remote
                // still thinks is held (use this any time a key gets stuck down).
                if let Some(pl) = &pl {
                    pl.release();
                }
                release_all_input(&w, &state);
                return glib::Propagation::Stop;
            }
            // Physical key identity: evdev keycode = GTK hardware_keycode - 8 (so games
            // that read raw keys — Minecraft/GLFW — get the right keys regardless of layout).
            let keycode = code.saturating_sub(8);
            state.pressed.borrow_mut().insert(keycode);
            send(&w, format!(r#"{{"kind":"key_code","keycode":{keycode},"pressed":true}}"#));
            glib::Propagation::Proceed
        });
    }
    {
        let (w, state) = (writer.clone(), state.clone());
        key.connect_key_released(move |_c, _keyval, code, _s| {
            let keycode = code.saturating_sub(8);
            state.pressed.borrow_mut().remove(&keycode);
            release_keycode(&w, keycode);
        });
    }
    window.add_controller(key);

    {
        let (w, state, window2) = (writer.clone(), state.clone(), window.clone());
        window.connect_is_active_notify(move |win| {
            if win.is_active() {
                if state.inside.get() {
                    grab_keys(&window2);
                }
            } else {
                ungrab_shortcuts(&window2);
                release_all_input(&w, &state);
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

/// Pick the richest MIME we'd want from an offer: image first, then HTML, then text.
fn pick_mime(mimes: &[String]) -> Option<String> {
    let pref = |m: &str| {
        if m.starts_with("image/png") { 0 }
        else if m.starts_with("image/") { 1 }
        else if m == "text/html" { 2 }
        else if is_text_mime(m) { 3 }
        else { 4 }
    };
    mimes.iter().min_by_key(|m| pref(m)).cloned()
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
        glib::timeout_add_local(Duration::from_millis(80), move || {
            let msgs: Vec<ClipboardMsg> = inbox.lock().unwrap().drain(..).collect();
            for msg in msgs {
                match msg {
                    ClipboardMsg::Offer(o) => {
                        if let Some(mime) = pick_mime(&o.mime_types) {
                            let req = ClipboardRequest { serial: o.serial, mime_type: mime };
                            if let Ok(json) = serde_json::to_string(&ClipboardMsg::Request(req)) {
                                send_tagged(&writer, 1, json);
                            }
                        }
                    }
                    ClipboardMsg::Data(d) => {
                        applying.set(true);
                        if is_text_mime(&d.mime_type) {
                            if let Ok(text) = String::from_utf8(d.bytes) {
                                clipboard.set_text(&text);
                            }
                        } else {
                            let bytes = glib::Bytes::from_owned(d.bytes);
                            let provider = gdk::ContentProvider::for_bytes(&d.mime_type, &bytes);
                            let _ = clipboard.set_content(Some(&provider));
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
    let reply = {
        let writer = writer.clone();
        move |mime: String, bytes: Vec<u8>| {
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
    if let Some(g) = writer.lock().unwrap().as_mut() {
        let hdr = (json.len() as u32).to_be_bytes();
        let _ = g
            .write_all(&[tag])
            .and_then(|_| g.write_all(&hdr))
            .and_then(|_| g.write_all(json.as_bytes()));
    }
}

fn send(writer: &Writer, json: String) {
    send_tagged(writer, 0, json);
}

/// GTK/X button number → evdev code.
fn evdev_button(n: u32) -> i32 {
    match n {
        1 => 0x110, // BTN_LEFT
        2 => 0x112, // BTN_MIDDLE
        3 => 0x111, // BTN_RIGHT
        _ => 0x110,
    }
}
