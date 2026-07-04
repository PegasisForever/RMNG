//! The per-node **computer-use MCP**, served over HTTP from inside the clone
//! (`RMNG_DAEMON_MCP_PORT`, default 9004). Replaces the old `computer-use`
//! stdio binary: the in-clone Claude agent connects here directly, and the
//! control-server's fleet MCP proxies to it.
//!
//! Stateless JSON-RPC (`initialize`/`ping`/`tools/list`/`tools/call`), the same
//! curl-testable shape the control-server MCP uses — no rmcp/SSE machinery. It
//! shares the daemon's live Mutter `rd` session (input injection) and the latest
//! captured dmabuf per monitor (on-demand screenshots, GPU-encoded via `media`).
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
/// `screenshot` tool dups the fd and GPU-encodes it to PNG via `media`.
pub struct LatestFrame {
    pub fd: OwnedFd,
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
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
    /// Last injected pointer position per monitor (for eased `mouse_move`).
    last_pos: Arc<Mutex<HashMap<u32, (f64, f64)>>>,
}

/// Serve the MCP over HTTP, reading the daemon's CURRENT Mutter session (`active`) + live
/// monitor set per request so it follows a live layout swap.
pub async fn serve(
    active: ActiveSession,
    live_monitors: Arc<Mutex<Vec<VirtualMonitor>>>,
    latest: LatestFrames,
    transport: Arc<Transport>,
    port: u16,
) -> anyhow::Result<()> {
    let state = McpState {
        active,
        live_monitors,
        latest,
        transport,
        last_pos: Arc::new(Mutex::new(HashMap::new())),
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

fn tools_list() -> Value {
    let mon = json!({ "monitor": { "type": "integer", "description": "monitor id (default: first)" } });
    let xy = json!({ "x": { "type": "number" }, "y": { "type": "number" }, "monitor": { "type": "integer" } });
    let mut t = vec![
        tool("list_monitors", "List the clone's virtual monitors (id, size)", json!({}), json!([])),
        tool("screenshot", "Capture a PNG screenshot of a monitor", mon.clone(), json!([])),
        tool("mouse_move", "Move the pointer to (x,y) with a smooth glide", xy.clone(), json!(["x", "y"])),
        tool("left_click", "Left-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool("right_click", "Right-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool("middle_click", "Middle-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool("left_double_click", "Double left-click (optionally glide to x,y first)", xy.clone(), json!([])),
        tool(
            "scroll",
            "Scroll vertically by `amount` notches (positive = down); optional x,y to glide to first",
            json!({ "amount": { "type": "integer" }, "x": { "type": "number" }, "y": { "type": "number" }, "monitor": { "type": "integer" } }),
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
            let list: Vec<Value> =
                mons_snapshot(st).iter().map(|m| json!({ "id": m.id, "width": m.width, "height": m.height })).collect();
            Ok(text(json!(list).to_string()))
        }
        "screenshot" => {
            let m = resolve_mon(&mons_snapshot(st), &args)?;
            Ok(image_content(&screenshot_png(st, &m)?))
        }
        "mouse_move" => {
            let m = resolve_mon(&mons_snapshot(st), &args)?;
            let (rd, _) = session_snapshot(st).await;
            let (x, y) = (n("x").ok_or("x required")?, n("y").ok_or("y required")?);
            ease_move(st, &rd, &m, x, y).await?;
            Ok(settle_shot(st, &m).await)
        }
        "left_click" => click(st, &args, BTN_LEFT, 1).await,
        "right_click" => click(st, &args, BTN_RIGHT, 1).await,
        "middle_click" => click(st, &args, BTN_MIDDLE, 1).await,
        "left_double_click" => click(st, &args, BTN_LEFT, 2).await,
        "scroll" => {
            let m = resolve_mon(&mons_snapshot(st), &args)?;
            let (rd, _) = session_snapshot(st).await;
            if n("x").is_some() && n("y").is_some() {
                ease_move(st, &rd, &m, n("x").unwrap(), n("y").unwrap()).await?;
            }
            let amount = args.get("amount").and_then(Value::as_i64).unwrap_or(0).clamp(-15, 15);
            let step = if amount >= 0 { 1 } else { -1 };
            for _ in 0..amount.abs() {
                rd.notify_pointer_axis_discrete(0, step as i32).await.map_err(e)?;
                sleep(SCROLL_STEP_MS).await;
            }
            Ok(settle_shot(st, &m).await)
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
            Ok(settle_shot(st, &first).await)
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
        "list_windows" | "move_window" | "list_apps" | "launch_app" => {
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
    let (rd, _) = session_snapshot(st).await;
    if let (Some(x), Some(y)) = (args.get("x").and_then(Value::as_f64), args.get("y").and_then(Value::as_f64)) {
        ease_move(st, &rd, &m, x, y).await?;
    }
    for i in 0..count {
        if i > 0 {
            sleep(DOUBLE_GAP_MS).await;
        }
        rd.notify_pointer_button(button, true).await.map_err(e)?;
        sleep(CLICK_PRESS_MS).await;
        rd.notify_pointer_button(button, false).await.map_err(e)?;
    }
    Ok(settle_shot(st, &m).await)
}

/// Glide the pointer to (tx,ty) over MOVE_STEPS eased steps, emitting a cursor warp
/// each step so the viewer animates the agent's move. `rd` is a caller-held snapshot of the
/// current session (so no session lock is held across the eased-motion awaits).
async fn ease_move(st: &McpState, rd: &RemoteDesktopSessionProxy<'static>, m: &Mon, tx: f64, ty: f64) -> Result<(), String> {
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
    let c = CursorMeta { monitor_id, x: x.round() as i32, y: y.round() as i32, shape: None, warp: true };
    let _ = st.transport.send(&DaemonMsg::Cursor(c), &[]);
}

fn clamp(m: &Mon, x: f64, y: f64) -> (f64, f64) {
    (x.clamp(0.0, (m.width.saturating_sub(1)) as f64), y.clamp(0.0, (m.height.saturating_sub(1)) as f64))
}

/// Encode the latest captured frame for `m` to PNG (GPU VPP → PNG via `media`).
fn screenshot_png(st: &McpState, m: &Mon) -> Result<Vec<u8>, String> {
    let (fd, fourcc, modifier, w, h) = {
        let latest = st.latest.lock().unwrap();
        let f = latest.get(&m.id).ok_or_else(|| format!("no frame captured yet for monitor {}", m.id))?;
        (dup(&f.fd).ok_or("dup failed")?, f.fourcc, f.modifier, f.width, f.height)
    };
    media::screenshot_png(fd, fourcc, modifier, w, h).map_err(|e| e.to_string())
}

/// Let the desktop repaint, then return a screenshot (best-effort → text on failure).
async fn settle_shot(st: &McpState, m: &Mon) -> Value {
    sleep(SETTLE_MS).await;
    match screenshot_png(st, m) {
        Ok(png) => image_content(&png),
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
fn image_content(png: &[u8]) -> Value {
    json!([{ "type": "image", "mimeType": "image/png", "data": base64(png) }])
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
