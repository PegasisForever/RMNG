//! The per-node **computer-use MCP**, served over HTTP from inside the clone
//! (`RMNG_DAEMON_MCP_PORT`, default 9004). Replaces the old `computer-use`
//! stdio binary: the in-clone Claude agent connects here directly, and the
//! control-server's web desktop proxy forwards operator calls to it.
//!
//! Stateless JSON-RPC (`initialize`/`ping`/`tools/list`/`tools/call`) with a
//! curl-testable HTTP shape — no rmcp/SSE machinery. It
//! shares the daemon's live Mutter `rd` session (input injection) and the latest
//! captured dmabuf per monitor (on-demand screenshots, GPU-encoded via `media`).
//!
//! ## Virtual coordinate space
//!
//! Screenshots and pointer coordinates share one **virtual** space, height-locked to
//! `RMNG_DESKTOP_HEIGHT` (default 1080) and aspect-preserving: a 2560×1440 monitor is
//! served as 1920×1080. Vision models are trained near 1080p, so a native-res screenshot
//! both wastes tokens and puts coordinates outside the model's comfort zone. Because the
//! image and the pointer space are derived from the same [`virt_dims`] call, they agree by
//! construction — the agent clicks what it sees, with no scaling knowledge on its side.
//!
//! [`to_native`] is the **single** conversion point ([`ease_move`]'s first line): everything
//! downstream of it (`clamp`, `emit_warp`, `last_pos`, `notify_pointer_motion_absolute`) is
//! native and unchanged. The operator's live-drive path (viewer → control-server → daemon
//! socket) never passes through here and stays native end to end.
//!
//! Window-management tools (`list_windows`/…) live in [`crate::windows`].

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use wire::socket::{CursorMeta, DaemonMsg};

use crate::ActiveSession;
use crate::keysym;
use crate::mutter::{RemoteDesktopSessionProxy, VirtualMonitor};
use crate::transport::Transport;
use crate::windows;

// Evdev button codes.
const BTN_LEFT: i32 = 0x110;
const BTN_RIGHT: i32 = 0x111;
const BTN_MIDDLE: i32 = 0x112;
// Motion easing + action timing (mirrors the old computer-use desktop tools).
const MOVE_STEPS: u32 = 10;
const MOVE_STEP_MS: u64 = 10; // 10 steps × 10 ms ≈ 100 ms glide
const CLICK_PRESS_MS: u64 = 50;
const DOUBLE_GAP_MS: u64 = 80;
const TYPE_KEY_MS: u64 = 12;
const SCROLL_STEP_MS: u64 = 25;
/// Let the desktop repaint before the post-action screenshot (damage-driven capture).
const SETTLE_MS: u64 = 350;

/// One captured dmabuf per monitor, refreshed by the capture callbacks; the
/// `screenshot` tool dups the fd and GPU-encodes it to JPEG via `media`.
pub struct LatestFrame {
    pub fd: OwnedFd,
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    /// Real per-plane (offset, stride) of the dmabuf, so the on-demand screenshot
    /// encoder imports it with the GPU-padded pitch (widths whose pitch isn't
    /// 16-aligned have stride ≠ width·4) instead of a fabricated one.
    pub planes: Vec<wire::socket::PlaneLayout>,
}
pub type LatestFrames = Arc<Mutex<HashMap<u32, LatestFrame>>>;

#[derive(Clone)]
struct Mon {
    id: u32,
    stream: String,
    width: u32,
    height: u32,
}

#[derive(Clone)]
struct McpState {
    /// The live session (input `rd` + session `conn`), swapped by `reconfigure`. Each
    /// handler snapshots `rd`/`conn` under a short-lived lock so it follows a swap — and,
    /// crucially, never pins the OLD `zbus::Connection` past a swap (holding it would block
    /// the clipboard signal-stream re-subscribe).
    active: ActiveSession,
    /// The live virtual-monitor set, refreshed by `reconfigure`. Snapshotted per request.
    live_monitors: Arc<Mutex<Vec<VirtualMonitor>>>,
    latest: LatestFrames,
    transport: Arc<Transport>,
    /// Last injected pointer position per monitor (for eased `mouse_move`). **Native** pixels,
    /// like everything downstream of [`to_native`].
    last_pos: Arc<Mutex<HashMap<u32, (f64, f64)>>>,
    /// Default virtual-space height (`RMNG_DESKTOP_HEIGHT`, 1080). `0` disables scaling.
    virt_height: u32,
}

/// Serve the MCP over HTTP, reading the daemon's CURRENT Mutter session (`active`) + live
/// monitor set per request so it follows a live layout swap.
pub async fn serve(
    active: ActiveSession,
    live_monitors: Arc<Mutex<Vec<VirtualMonitor>>>,
    latest: LatestFrames,
    transport: Arc<Transport>,
    port: u16,
    virt_height: u32,
) -> anyhow::Result<()> {
    let state = McpState {
        active,
        live_monitors,
        latest,
        transport,
        last_pos: Arc::new(Mutex::new(HashMap::new())),
        virt_height,
    };
    let app = Router::new().route("/", post(rpc)).with_state(state);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("clone-daemon MCP on http://{addr}");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

/// Snapshot the CURRENT session's `rd` + `conn` under a short-lived lock, then drop the
/// guard — so the long `notify_*` / `windows::call` awaits never hold the session lock, and
/// the OLD conn is free to drop once `reconfigure` repoints `active`.
async fn session_snapshot(st: &McpState) -> (RemoteDesktopSessionProxy<'static>, zbus::Connection) {
    let rt = st.active.lock().await;
    (rt.rd.clone(), rt.conn.clone())
}

// --- virtual coordinate space ------------------------------------------------

/// Round down to even. NV12/I420 are 4:2:0, so odd dimensions have no valid chroma plane
/// and `vapostproc` refuses the caps outright.
fn round_even(n: u32) -> u32 {
    n & !1
}

/// Parse a `"<W>x<H>"` resolution string. Rejects zero, non-numeric, and missing-separator
/// forms so a typo falls back to the default rather than silently scaling to nonsense.
fn parse_wxh(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// The virtual `(width, height)` for a monitor of native `(nw, nh)`: the size the screenshot
/// comes out at *and* the space `x`/`y` are read in.
///
/// `res` is a per-call `resolution` override — `"native"`, a `"<W>x<H>"` pair, or `None` for
/// the daemon default. The default is height-locked and aspect-preserving (2560×1440 →
/// 1920×1080; ultrawide 3440×1440 → 2580×1080), and **never upscales** — a monitor already at
/// or below `virt_height` is served natively. Results are even and at least 2×2, so
/// [`to_native`] can never divide by zero.
fn virt_dims(virt_height: u32, nw: u32, nh: u32, res: Option<&str>) -> (u32, u32) {
    // A degenerate monitor (smaller than one chroma block) has no sane virtual space; hand back
    // native and let the existing native `clamp` deal with it. Returning early also means the
    // `.max(2)` floor below can never exceed the monitor's own size.
    if nw < 2 || nh < 2 {
        return (nw, nh);
    }
    let fit = |w: u32, h: u32| (round_even(w.min(nw)).max(2), round_even(h.min(nh)).max(2));
    match res {
        Some("native") => (nw, nh),
        // An unparseable override falls back to native rather than the 1080p default: the
        // caller explicitly asked for a specific space, so silently substituting a *different*
        // scaled one would misplace their clicks. Native is the identity mapping.
        Some(s) => parse_wxh(s).map(|(w, h)| fit(w, h)).unwrap_or((nw, nh)),
        None => {
            if virt_height == 0 || virt_height >= nh {
                (nw, nh)
            } else {
                fit(nw * virt_height / nh, virt_height)
            }
        }
    }
}

/// Resolve the virtual dims for one tool call: the monitor's native size, the daemon default,
/// and the call's own optional `resolution` argument.
fn call_dims(st: &McpState, m: &Mon, args: &Value) -> (u32, u32) {
    virt_dims(st.virt_height, m.width, m.height, args.get("resolution").and_then(Value::as_str))
}

/// Virtual `(x, y)` → native, per axis. Per-axis (not one shared factor) is deliberate: `vw` is
/// even-rounded independently of `vh`, so `nw/vw ≠ nh/vh` in general, and mapping each axis
/// across its own full range keeps the far edge reachable. Rounding is left to the callers
/// (`ease_move` interpolates in f64; `emit_warp` rounds once at the end).
fn to_native(m: &Mon, vw: u32, vh: u32, x: f64, y: f64) -> (f64, f64) {
    (x * m.width as f64 / vw as f64, y * m.height as f64 / vh as f64)
}

/// Snapshot the live monitor set as `Mon`s under a short-lived lock.
fn mons_snapshot(st: &McpState) -> Vec<Mon> {
    st.live_monitors
        .lock()
        .unwrap()
        .iter()
        .map(|m| Mon { id: m.monitor_id, stream: m.stream_path.clone(), width: m.width, height: m.height })
        .collect()
}

// --- JSON-RPC plumbing ------------------------------------------------------

async fn rpc(State(st): State<McpState>, Json(req): Json<Value>) -> Json<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    let result: Result<Value, String> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "clone-daemon-mcp", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_list() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            call_tool(&st, &name, args).await.map(|content| json!({ "content": content }))
        }
        other => Err(format!("unknown method '{other}'")),
    };
    match result {
        Ok(v) => Json(json!({ "jsonrpc": "2.0", "id": id, "result": v })),
        Err(e) => Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": e } })),
    }
}

fn tool(name: &str, desc: &str, props: Value, required: Value) -> Value {
    json!({ "name": name, "description": desc, "inputSchema": { "type": "object", "properties": props, "required": required } })
}

/// The `resolution` argument, shared by `screenshot` and every pointer tool. Optional; omit it
/// and you get the daemon's default virtual space.
fn resolution_prop() -> Value {
    json!({
        "type": "string",
        "description": "Coordinate + screenshot space for this call: \"<W>x<H>\" (e.g. \"1280x720\") \
                        or \"native\". Omit for the default 1920x1080. A pointer call's value must \
                        match the screenshot you are reading coordinates off — calls are stateless, \
                        so the server cannot infer it.",
    })
}

fn tools_list() -> Value {
    let coord_desc = "pixels in the screenshot's own space (top-left origin), which is \
                      1920x1080 by default — not the monitor's native resolution. See list_monitors.";
    let mon = json!({
        "monitor": { "type": "integer", "description": "monitor id (default: first)" },
        "resolution": resolution_prop(),
    });
    let xy = json!({
        "x": { "type": "number", "description": coord_desc },
        "y": { "type": "number", "description": coord_desc },
        "monitor": { "type": "integer" },
        "resolution": resolution_prop(),
    });
    let mut t = vec![
        tool(
            "list_monitors",
            "List the clone's virtual monitors: id, the width/height coordinates and screenshots \
             use, and the underlying native_width/native_height",
            json!({}),
            json!([]),
        ),
        tool(
            "screenshot",
            "Capture a JPEG screenshot of a monitor (1920x1080 by default; pass x/y back in that \
             same space)",
            mon.clone(),
            json!([]),
        ),
        tool("mouse_move", "Move the pointer to (x,y) with a smooth glide", xy.clone(), json!(["x", "y"])),
        tool("left_click", "Left-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool("right_click", "Right-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool("middle_click", "Middle-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool("left_double_click", "Double left-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool(
            "scroll",
            "Scroll vertically by `amount` notches (positive = down); optional x,y to glide to first",
            json!({
                "amount": { "type": "integer" },
                "x": { "type": "number", "description": coord_desc },
                "y": { "type": "number", "description": coord_desc },
                "monitor": { "type": "integer" },
                "resolution": resolution_prop(),
            }),
            json!(["amount"]),
        ),
        tool("key", "Press a key combo, e.g. \"ctrl+c\", \"Return\", \"alt+Tab\"", json!({ "keys": { "type": "string" } }), json!(["keys"])),
        tool("type", "Type a unicode string", json!({ "text": { "type": "string" } }), json!(["text"])),
    ];
    t.extend(windows::tools());
    Value::Array(t)
}

async fn call_tool(st: &McpState, name: &str, args: Value) -> Result<Value, String> {
    let n = |k: &str| args.get(k).and_then(Value::as_f64);
    match name {
        "list_monitors" => {
            // `width`/`height` are the space screenshots and x/y use; the native size is
            // reported alongside it so a caller that wants full res knows what to ask for.
            let list: Vec<Value> = mons_snapshot(st)
                .iter()
                .map(|m| {
                    let (vw, vh) = call_dims(st, m, &json!({}));
                    json!({
                        "id": m.id,
                        "width": vw,
                        "height": vh,
                        "native_width": m.width,
                        "native_height": m.height,
                    })
                })
                .collect();
            Ok(text(json!(list).to_string()))
        }
        "screenshot" => {
            let m = resolve_mon(&mons_snapshot(st), &args)?;
            let (vw, vh) = call_dims(st, &m, &args);
            Ok(image_content(&screenshot_jpeg(st, &m, vw, vh)?))
        }
        "mouse_move" => {
            let m = resolve_mon(&mons_snapshot(st), &args)?;
            // One `call_dims` for both the move and the settle shot, so a per-call
            // `resolution` and the image it returns can never disagree.
            let (vw, vh) = call_dims(st, &m, &args);
            let (rd, _) = session_snapshot(st).await;
            let (x, y) = (n("x").ok_or("x required")?, n("y").ok_or("y required")?);
            ease_move(st, &rd, &m, vw, vh, x, y).await?;
            Ok(settle_shot(st, &m, vw, vh).await)
        }
        "left_click" => click(st, &args, BTN_LEFT, 1).await,
        "right_click" => click(st, &args, BTN_RIGHT, 1).await,
        "middle_click" => click(st, &args, BTN_MIDDLE, 1).await,
        "left_double_click" => click(st, &args, BTN_LEFT, 2).await,
        "scroll" => {
            let m = resolve_mon(&mons_snapshot(st), &args)?;
            let (vw, vh) = call_dims(st, &m, &args);
            let (rd, _) = session_snapshot(st).await;
            if n("x").is_some() && n("y").is_some() {
                ease_move(st, &rd, &m, vw, vh, n("x").unwrap(), n("y").unwrap()).await?;
            }
            let amount = args.get("amount").and_then(Value::as_i64).unwrap_or(0).clamp(-15, 15);
            let step = if amount >= 0 { 1 } else { -1 };
            for _ in 0..amount.abs() {
                rd.notify_pointer_axis_discrete(0, step as i32).await.map_err(e)?;
                sleep(SCROLL_STEP_MS).await;
            }
            Ok(settle_shot(st, &m, vw, vh).await)
        }
        "key" => {
            let (rd, _) = session_snapshot(st).await;
            let combo = args.get("keys").and_then(Value::as_str).ok_or("keys required")?;
            let syms = keysym::parse_key_combo(combo).map_err(|e| e.to_string())?;
            for &s in &syms {
                rd.notify_keyboard_keysym(s, true).await.map_err(e)?;
            }
            for &s in syms.iter().rev() {
                rd.notify_keyboard_keysym(s, false).await.map_err(e)?;
            }
            let first = mons_snapshot(st).into_iter().next().ok_or("no monitors")?;
            // No coordinates involved, so the settle shot just uses the default space.
            let (vw, vh) = call_dims(st, &first, &json!({}));
            Ok(settle_shot(st, &first, vw, vh).await)
        }
        "type" => {
            let (rd, _) = session_snapshot(st).await;
            let txt = args.get("text").and_then(Value::as_str).ok_or("text required")?;
            for ch in txt.chars() {
                let Some(ks) = keysym::char_to_keysym(ch) else { continue };
                rd.notify_keyboard_keysym(ks, true).await.map_err(e)?;
                rd.notify_keyboard_keysym(ks, false).await.map_err(e)?;
                sleep(TYPE_KEY_MS).await;
            }
            Ok(text(format!("typed {} chars", txt.chars().count())))
        }
        // Window management (gnome-shell Eval).
        "list_windows" | "move_window" => {
            let (_, conn) = session_snapshot(st).await;
            windows::call(&conn, name, &args).await
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

// --- desktop actions --------------------------------------------------------

/// A click of `count` presses of `button`, optionally moving to x,y first. Snapshots the
/// CURRENT session's `rd` once, then acts (no session lock held across the presses).
async fn click(st: &McpState, args: &Value, button: i32, count: u32) -> Result<Value, String> {
    let m = resolve_mon(&mons_snapshot(st), args)?;
    let (vw, vh) = call_dims(st, &m, args);
    let (rd, _) = session_snapshot(st).await;
    if let (Some(x), Some(y)) = (args.get("x").and_then(Value::as_f64), args.get("y").and_then(Value::as_f64)) {
        ease_move(st, &rd, &m, vw, vh, x, y).await?;
    }
    for i in 0..count {
        if i > 0 {
            sleep(DOUBLE_GAP_MS).await;
        }
        rd.notify_pointer_button(button, true).await.map_err(e)?;
        sleep(CLICK_PRESS_MS).await;
        rd.notify_pointer_button(button, false).await.map_err(e)?;
    }
    Ok(settle_shot(st, &m, vw, vh).await)
}

/// Glide the pointer to (tx,ty) over MOVE_STEPS eased steps, emitting a cursor warp
/// each step so the viewer animates the agent's move. `rd` is a caller-held snapshot of the
/// current session (so no session lock is held across the eased-motion awaits).
///
/// `(tx, ty)` arrive in the **virtual** space described by `(vw, vh)`; the first line converts
/// them to native and everything after — clamp, easing, warp, injection — is native. This is
/// the single conversion point for all three callers (`mouse_move`, `click`, `scroll`).
async fn ease_move(
    st: &McpState,
    rd: &RemoteDesktopSessionProxy<'static>,
    m: &Mon,
    vw: u32,
    vh: u32,
    tx: f64,
    ty: f64,
) -> Result<(), String> {
    let (tx, ty) = to_native(m, vw, vh, tx, ty);
    let (tx, ty) = clamp(m, tx, ty);
    let (sx, sy) = st.last_pos.lock().unwrap().get(&m.id).copied().unwrap_or((tx, ty));
    for i in 1..=MOVE_STEPS {
        let t = i as f64 / MOVE_STEPS as f64;
        let ease = if t < 0.5 { 2.0 * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(2) / 2.0 }; // ease-in-out quad
        let (x, y) = (sx + (tx - sx) * ease, sy + (ty - sy) * ease);
        rd.notify_pointer_motion_absolute(&m.stream, x, y).await.map_err(e)?;
        emit_warp(st, m.id, x, y);
        sleep(MOVE_STEP_MS).await;
    }
    *st.last_pos.lock().unwrap().entry(m.id).or_default() = (tx, ty);
    Ok(())
}

/// Tell the viewer the cursor warped here (agent-driven) so it snaps + suppresses
/// the user's local motion briefly (see the viewer's WarpSuppress).
fn emit_warp(st: &McpState, monitor_id: u32, x: f64, y: f64) {
    let c = CursorMeta { monitor_id, x: x.round() as i32, y: y.round() as i32, shape: None, warp: true, hidden: false };
    let _ = st.transport.send(&DaemonMsg::Cursor(c), &[]);
}

fn clamp(m: &Mon, x: f64, y: f64) -> (f64, f64) {
    (x.clamp(0.0, (m.width.saturating_sub(1)) as f64), y.clamp(0.0, (m.height.saturating_sub(1)) as f64))
}

/// Encode the latest captured frame for `m` to JPEG (GPU VPP → JPEG via `media`), scaled to the
/// virtual size `(vw, vh)`. `w`/`h` from the frame stay native: they describe the dmabuf being
/// imported, and only the pipeline's output caps carry the virtual size.
fn screenshot_jpeg(st: &McpState, m: &Mon, vw: u32, vh: u32) -> Result<Vec<u8>, String> {
    let (fd, fourcc, modifier, w, h, planes) = {
        let latest = st.latest.lock().unwrap();
        let f = latest.get(&m.id).ok_or_else(|| format!("no frame captured yet for monitor {}", m.id))?;
        (dup(&f.fd).ok_or("dup failed")?, f.fourcc, f.modifier, f.width, f.height, f.planes.clone())
    };
    media::screenshot_jpeg(fd, fourcc, modifier, w, h, &planes, vw, vh).map_err(|e| e.to_string())
}

/// Let the desktop repaint, then return a screenshot (best-effort → text on failure). Takes the
/// caller's `(vw, vh)` so the settle image is in the same space as the coordinates that produced it.
async fn settle_shot(st: &McpState, m: &Mon, vw: u32, vh: u32) -> Value {
    sleep(SETTLE_MS).await;
    match screenshot_jpeg(st, m, vw, vh) {
        Ok(jpeg) => image_content(&jpeg),
        Err(_) => text("ok"),
    }
}

fn resolve_mon(mons: &[Mon], args: &Value) -> Result<Mon, String> {
    match args.get("monitor").and_then(Value::as_u64) {
        Some(id) => mons.iter().find(|m| m.id as u64 == id).cloned().ok_or_else(|| format!("no monitor {id}")),
        None => mons.first().cloned().ok_or_else(|| "no monitors".into()),
    }
}

// --- small helpers ----------------------------------------------------------

async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
/// zbus error → String (for `.map_err`).
fn e(err: zbus::Error) -> String {
    err.to_string()
}
fn dup(fd: &OwnedFd) -> Option<OwnedFd> {
    let raw = nix::unistd::dup(fd.as_raw_fd()).ok()?;
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}
fn text(s: impl Into<String>) -> Value {
    json!([{ "type": "text", "text": s.into() }])
}
fn image_content(jpeg: &[u8]) -> Value {
    json!([{ "type": "image", "mimeType": "image/jpeg", "data": base64(jpeg) }])
}

/// Minimal standard base64 encode (screenshot image content).
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default virtual space: height-locked to 1080, aspect preserved, even, never upscaled.
    #[test]
    fn virt_dims_default_is_height_locked_and_aspect_preserving() {
        // 16:9 lands exactly on 1920×1080 — the case that matters most.
        assert_eq!(virt_dims(1080, 2560, 1440, None), (1920, 1080));
        assert_eq!(virt_dims(1080, 3840, 2160, None), (1920, 1080));
        // Ultrawide keeps its aspect rather than being squeezed into 16:9.
        assert_eq!(virt_dims(1080, 3440, 1440, None), (2580, 1080));
        // 16:10 → 1728×1080, still even.
        assert_eq!(virt_dims(1080, 2560, 1600, None), (1728, 1080));
    }

    #[test]
    fn virt_dims_never_upscales() {
        // Already at the target height, or below it → native, untouched.
        assert_eq!(virt_dims(1080, 1920, 1080, None), (1920, 1080));
        assert_eq!(virt_dims(1080, 1280, 720, None), (1280, 720));
        // `RMNG_DESKTOP_HEIGHT=0` disables scaling entirely.
        assert_eq!(virt_dims(0, 2560, 1440, None), (2560, 1440));
    }

    #[test]
    fn virt_dims_honors_per_call_overrides() {
        assert_eq!(virt_dims(1080, 2560, 1440, Some("native")), (2560, 1440));
        assert_eq!(virt_dims(1080, 2560, 1440, Some("1280x720")), (1280, 720));
        assert_eq!(virt_dims(1080, 2560, 1440, Some("1280X720")), (1280, 720));
        // Odd dimensions round down to even (4:2:0 chroma).
        assert_eq!(virt_dims(1080, 2560, 1440, Some("1281x721")), (1280, 720));
        // An override is capped at native — the pipeline downscales, it doesn't upscale.
        assert_eq!(virt_dims(1080, 2560, 1440, Some("3840x2160")), (2560, 1440));
    }

    /// A malformed override falls back to *native*, not to the 1080p default: the caller asked
    /// for one specific space, so quietly substituting a different scaled one would land their
    /// clicks in the wrong place. Native at least keeps the mapping honest (identity).
    #[test]
    fn virt_dims_rejects_malformed_overrides_to_native() {
        for bad in ["", "garbage", "1920", "1920x", "x1080", "0x1080", "1920x0", "-1x-1", "axb"] {
            assert_eq!(virt_dims(1080, 2560, 1440, Some(bad)), (2560, 1440), "input {bad:?}");
        }
    }

    /// `to_native` divides by the virtual dims, so they must never be 0 — including for the
    /// pathological inputs a caller can actually reach via `resolution`.
    #[test]
    fn virt_dims_never_returns_zero() {
        for res in [None, Some("1x1"), Some("2x1"), Some("0x0"), Some("native")] {
            let (vw, vh) = virt_dims(1080, 2560, 1440, res);
            assert!(vw >= 2 && vh >= 2, "res {res:?} produced {vw}x{vh}");
        }
        // A tiny monitor can't be clamped up past its own native size.
        assert_eq!(virt_dims(1080, 1, 1, Some("1280x720")), (1, 1));
    }

    #[test]
    fn to_native_maps_the_full_range_per_axis() {
        let m = Mon { id: 0, stream: String::new(), width: 2560, height: 1440 };
        // Origin is fixed, centre maps to centre, and the far edge reaches the far edge.
        assert_eq!(to_native(&m, 1920, 1080, 0.0, 0.0), (0.0, 0.0));
        assert_eq!(to_native(&m, 1920, 1080, 960.0, 540.0), (1280.0, 720.0));
        assert_eq!(to_native(&m, 1920, 1080, 1920.0, 1080.0), (2560.0, 1440.0));
        // The bottom-right *pixel* (1919,1079) lands just inside native, where `clamp` takes over.
        let (x, y) = to_native(&m, 1920, 1080, 1919.0, 1079.0);
        assert!(x < 2560.0 && y < 1440.0);
        // A native-space call is the identity mapping.
        assert_eq!(to_native(&m, 2560, 1440, 100.0, 200.0), (100.0, 200.0));
    }
}
