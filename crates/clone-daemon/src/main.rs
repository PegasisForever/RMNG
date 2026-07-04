//! `clone-daemon` (Phase 3) — the thin in-clone capture + input pipe.
//!
//! Two run modes:
//!   - `RMNG_SOCKET=<path>` → **shipping**: connect to control-server's media
//!     socket, ship each monitor's dmabuf (FrameMsg + fds via SCM_RIGHTS), and
//!     inject incoming input via Mutter RemoteDesktop.
//!   - otherwise → **capture self-test**: log fourcc/modifier/size + fps (cursor
//!     nudge generates damage on the static headless desktop).

mod capture;
mod capture_pw;
mod clipboard;
mod detector;
mod keysym;
mod mcp;
mod mutter;
mod transport;
mod windows;

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use wire::socket::{
    CursorMeta, CursorShape, DaemonMsg, FrameMsg, InputMsg, MonitorPlacement, PlaneLayout, ServerMsg,
};

use crate::mutter::Session;

/// The session-bound state the input/clipboard tasks need, swapped atomically by the
/// reconfigure controller. `rd` injects input; `streams` maps monitor_id → stream path
/// for absolute-pointer motion (`notify_pointer_motion_absolute(stream, x, y)`).
///
/// Input reads this per event and clipboard reads it per selection op, so a
/// make-before-break swap (see `reconfigure`) re-points both at the new session by
/// mutating this one handle. `pub(crate)` so `clipboard::run` can hold the same handle.
pub(crate) struct SessionRuntime {
    pub(crate) rd: mutter::RemoteDesktopSessionProxy<'static>,
    pub(crate) streams: std::collections::HashMap<u32, String>,
}
pub(crate) type ActiveSession = std::sync::Arc<tokio::sync::Mutex<SessionRuntime>>;

/// One configured monitor: size, position (unified-desktop px) + primary flag.
#[derive(Clone, Copy, PartialEq)]
struct MonitorCfg {
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    primary: bool,
}

/// Parse `RMNG_MONITORS` = `WxH+X+Y[*]` comma-separated (X/Y default 0, trailing `*` =
/// primary). Empty → a single default 1080p primary. Guarantees exactly one primary.
fn parse_monitors(spec: Option<String>) -> Vec<MonitorCfg> {
    let mut mons: Vec<MonitorCfg> = spec
        .unwrap_or_default()
        .split(',')
        .filter_map(|tok| {
            let tok = tok.trim();
            if tok.is_empty() {
                return None;
            }
            let primary = tok.ends_with('*');
            let tok = tok.trim_end_matches('*');
            let (wh, pos) = match tok.split_once('+') {
                Some((wh, p)) => (wh, Some(p)),
                None => (tok, None),
            };
            let (w, h) = wh.split_once('x')?;
            let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
            let (x, y) = match pos {
                Some(p) => {
                    let (px, py) = p.split_once('+')?;
                    (px.trim().parse().ok()?, py.trim().parse().ok()?)
                }
                None => (0, 0),
            };
            Some(MonitorCfg { w, h, x, y, primary })
        })
        .collect();
    if mons.is_empty() {
        mons.push(MonitorCfg { w: 1920, h: 1080, x: 0, y: 0, primary: true });
    }
    if !mons.iter().any(|m| m.primary) {
        mons[0].primary = true;
    }
    mons
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // `clip` (the clipboard bridge) logs debug by default: copy/paste-driven
                // only (sparse), and the go-to trail for cross-machine clipboard issues.
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,clip=debug")),
        )
        .init();

    // Subcommands run as thin clients (no Mutter session / GStreamer): the needs-human
    // detector, ported from the retired `computer-use` binary. The agent arms
    // `wait-for-stuck` as a background task and `report-detection`s wrong verdicts.
    let mut argv = std::env::args().skip(1);
    if let Some(sub) = argv.next() {
        let rest: Vec<String> = argv.collect();
        return match sub.as_str() {
            "wait-for-stuck" => run_wait_for_stuck(rest).await,
            "report-detection" => run_report_detection(rest).await,
            other => anyhow::bail!("unknown subcommand {other:?} (want: wait-for-stuck | report-detection)"),
        };
    }

    gstreamer::init()?;
    let monitors = parse_monitors(std::env::var("RMNG_MONITORS").ok());
    let sizes: Vec<(u32, u32)> = monitors.iter().map(|m| (m.w, m.h)).collect();
    let socket = std::env::var("RMNG_SOCKET").ok();

    // Client-drawn cursor is the default for shipping: capture in cursor-mode
    // METADATA (cursor out-of-band via SPA_META_Cursor, read by the raw-PW path)
    // so the viewer draws it locally. RMNG_EMBEDDED_CURSOR=1 forces the
    // GStreamer/embedded-cursor path (cursor composited into the frame). The
    // standalone capture self-test always uses embedded (GStreamer).
    let embedded = std::env::var("RMNG_EMBEDDED_CURSOR").is_ok() || socket.is_none();
    let cursor_mode =
        if embedded { mutter::CURSOR_MODE_EMBEDDED } else { mutter::CURSOR_MODE_METADATA };
    match socket {
        Some(path) => {
            // Connect to the media socket FIRST (retrying while it's unavailable), THEN
            // build the expensive Mutter session — so a down/restarting control-server
            // doesn't churn sessions on every retry. On a later disconnect we exit and
            // systemd restarts us back into this cheap connect-retry loop.
            let transport = connect_retry(&path).await;
            tracing::info!(?sizes, embedded, "clone-daemon: setting up Mutter session");
            let session = mutter::setup_with_cursor_mode(&sizes, cursor_mode).await?;
            tracing::info!("session ready: {} virtual monitor(s)", session.monitors.len());
            apply_layout(&monitors).await;
            run_shipping(session, transport, &path, &monitors, embedded).await
        }
        None => {
            tracing::info!(?sizes, embedded, "clone-daemon: setting up Mutter session");
            let session = mutter::setup_with_cursor_mode(&sizes, cursor_mode).await?;
            tracing::info!("session ready: {} virtual monitor(s)", session.monitors.len());
            run_capture_test(session).await
        }
    }
}

/// `wait-for-stuck [--inference-url <url>] [--ignore-reason <str>]… [--interval <secs>]
/// [--timeout <secs>]` — flags match the old `computer-use` CLI so the agent's command
/// pattern carries over with just a binary-name swap.
async fn run_wait_for_stuck(args: Vec<String>) -> Result<()> {
    let mut inference_url = detector::default_inference_url();
    let mut ignore_reasons = Vec::new();
    let mut interval = 60u64;
    let mut timeout = 1200u64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--inference-url" => {
                inference_url = args.get(i + 1).context("--inference-url requires a URL")?.clone();
                i += 2;
            }
            "--ignore-reason" => {
                ignore_reasons.push(args.get(i + 1).context("--ignore-reason requires a string")?.clone());
                i += 2;
            }
            "--interval" => {
                interval = args.get(i + 1).context("--interval requires seconds")?.parse().context("--interval integer")?;
                i += 2;
            }
            "--timeout" => {
                timeout = args.get(i + 1).context("--timeout requires seconds")?.parse().context("--timeout integer")?;
                i += 2;
            }
            other => anyhow::bail!("unknown wait-for-stuck arg {other:?}"),
        }
    }
    let mcp_port =
        std::env::var("RMNG_DAEMON_MCP_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(9004);
    detector::wait_for_stuck(detector::WaitOptions {
        inference_url,
        ignore_reasons,
        interval: Duration::from_secs(interval),
        timeout: Duration::from_secs(timeout),
        mcp_port,
    })
    .await
}

/// `report-detection --kind false-positive|false-negative [--note <str>] [--control <url>]`.
async fn run_report_detection(args: Vec<String>) -> Result<()> {
    let mut false_positive = None;
    let mut note = String::new();
    let mut control_url = detector::default_control_url();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--kind" => {
                let v = args.get(i + 1).context("--kind requires false-positive|false-negative")?;
                false_positive = Some(match v.as_str() {
                    "false-positive" => true,
                    "false-negative" => false,
                    other => anyhow::bail!("--kind must be false-positive|false-negative, got {other:?}"),
                });
                i += 2;
            }
            "--note" => {
                note = args.get(i + 1).context("--note requires a string")?.clone();
                i += 2;
            }
            "--control" => {
                control_url = args.get(i + 1).context("--control requires a base URL")?.clone();
                i += 2;
            }
            other => anyhow::bail!("unknown report-detection arg {other:?}"),
        }
    }
    let false_positive =
        false_positive.context("report-detection requires --kind false-positive|false-negative")?;
    detector::report_detection(detector::ReportOptions { false_positive, note, control_url }).await
}

/// Connect to the media socket, retrying every second until it's reachable (the
/// control-server may be down or restarting). Building the Mutter session only after we
/// connect means a down server doesn't churn sessions on every retry.
async fn connect_retry(socket_path: &str) -> Arc<transport::Transport> {
    let mut warned = false;
    loop {
        match transport::Transport::connect(socket_path) {
            Ok(t) => return Arc::new(t),
            Err(e) => {
                if !warned {
                    tracing::warn!("media socket {socket_path} unavailable ({e}); retrying every 1s");
                    warned = true;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Apply the configured monitor layout (positions + primary) to Mutter via
/// `DisplayConfig.ApplyMonitorsConfig`. Uses `gdbus` (the nested ApplyMonitorsConfig
/// GVariant types are painful via zbus). Best-effort: logs + continues on failure (the
/// monitors still capture, just in Mutter's default left-to-right order). Connector names
/// are read back from `GetCurrentState` (Task 3.2) rather than assumed `Meta-<i>`, since
/// after a session-swap reconfiguration Mutter may hand out connectors in a different
/// order (or reuse different ones) than creation order.
async fn apply_layout(monitors: &[MonitorCfg]) {
    let Some((serial, stdout)) = get_current_state().await else {
        tracing::warn!("layout: couldn't read DisplayConfig state; leaving Mutter's default layout");
        return;
    };
    let mut available = parse_connectors(&stdout);
    let mut lm = String::from("[");
    let mut first = true;
    for m in monitors {
        // Pick (and consume) the first available connector whose current mode matches
        // this monitor's (w, h), so duplicate sizes still map 1:1 in order.
        let Some(idx) = available.iter().position(|(_, w, h)| *w == m.w && *h == m.h) else {
            tracing::warn!("layout: no Mutter connector currently at {}x{}; skipping that monitor", m.w, m.h);
            continue;
        };
        let (connector, w, h) = available.remove(idx);
        if !first {
            lm.push_str(", ");
        }
        first = false;
        // (x, y, scale, transform, primary, [(connector, mode_id, props)])
        lm.push_str(&format!(
            "({}, {}, 1.0, uint32 0, {}, [('{}', '{}x{}@60.000', @a{{sv}} {{}})])",
            m.x, m.y, m.primary, connector, w, h
        ));
    }
    lm.push(']');
    let out = tokio::process::Command::new("gdbus")
        .args([
            "call", "--session", "--dest", "org.gnome.Mutter.DisplayConfig",
            "--object-path", "/org/gnome/Mutter/DisplayConfig",
            "--method", "org.gnome.Mutter.DisplayConfig.ApplyMonitorsConfig",
            &serial.to_string(), "1", &lm, "@a{sv} {}",
        ])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => tracing::info!("applied monitor layout ({} monitor(s))", monitors.len()),
        Ok(o) => tracing::warn!("ApplyMonitorsConfig failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => tracing::warn!("ApplyMonitorsConfig spawn failed (gdbus missing?): {e}"),
    }
}

/// Call `DisplayConfig.GetCurrentState` and return `(serial, stdout)`. Used by
/// `apply_layout` to read back connector names from the current Mutter state.
async fn get_current_state() -> Option<(u32, String)> {
    let out = tokio::process::Command::new("gdbus")
        .args([
            "call", "--session", "--dest", "org.gnome.Mutter.DisplayConfig",
            "--object-path", "/org/gnome/Mutter/DisplayConfig",
            "--method", "org.gnome.Mutter.DisplayConfig.GetCurrentState",
        ])
        .output()
        .await
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let idx = s.find("uint32 ")?;
    let serial =
        s[idx + 7..].split(|c: char| !c.is_ascii_digit()).find(|t| !t.is_empty())?.parse().ok()?;
    Some((serial, s))
}

/// Parse the text form of `DisplayConfig.GetCurrentState`'s stdout (as printed by
/// `gdbus call`) into `(connector, width, height)` for each monitor's *current* mode.
///
/// Shape (scales list length varies, `...` elided):
/// ```text
/// (uint32 3, [(('Meta-0', 'MetaVendor', 'Virtual remote monitor', '0x000001'),
///   [('2560x1440@60.000', 2560, 1440, 60.0, 1.0, [1.0, 1.25],
///     {'is-current': <true>, 'is-preferred': <true>})],
///   {'is-builtin': <false>, ...}), (('Meta-1', ...), [...], {...})], [...], {...})
/// ```
/// Each monitor group starts at a `('Meta-` (or other connector-prefixed) tuple; we split
/// the monitor-list on that boundary. Within a group, the connector is the first
/// single-quoted string; the current mode is whichever mode tuple's props contain
/// `'is-current': <true>`, and its `WxH` are the two integers immediately following that
/// mode's `'WxH@rate'` id string.
fn parse_connectors(get_current_state_stdout: &str) -> Vec<(String, u32, u32)> {
    let mut out = Vec::new();
    // Split into per-monitor chunks on `(('` (the start of each monitor's connector
    // tuple), which only occurs at monitor-group boundaries in this shape.
    for chunk in get_current_state_stdout.split("((").skip(1) {
        // The connector name is the first single-quoted string in the chunk.
        let Some(connector) = first_quoted(chunk) else { continue };
        // Find the mode tuple containing `'is-current': <true>`; walk each
        // `'<mode-id>', <w>, <h>` occurrence and keep the one whose nearby props (up to
        // the next mode/group) mention is-current.
        let Some(current_mode_end) = chunk.find("'is-current': <true>") else { continue };
        // The mode id governing that is-current marker is the *last* mode-id string
        // (`'WxH@rate'`) appearing before it.
        let mut best: Option<(usize, u32, u32)> = None;
        let mut rest = chunk;
        let mut base = 0usize;
        while let Some(rel) = rest.find('\'') {
            let abs = base + rel;
            if abs >= current_mode_end {
                break;
            }
            let after_quote = &rest[rel + 1..];
            let Some(end_quote) = after_quote.find('\'') else { break };
            let id = &after_quote[..end_quote];
            // A mode id looks like `WxH@rate`; parse w/h if it matches, else skip (e.g.
            // connector/vendor/product/serial strings).
            if let Some((w, h)) = parse_mode_id(id) {
                best = Some((abs, w, h));
            }
            let consumed = rel + 1 + end_quote + 1;
            rest = &rest[consumed..];
            base = abs + 1 + end_quote + 1;
        }
        if let Some((_, w, h)) = best {
            out.push((connector, w, h));
        }
    }
    out
}

/// The first single-quoted string in `s`, e.g. `"'Meta-0', 'MetaVendor'"` → `"Meta-0"`.
fn first_quoted(s: &str) -> Option<String> {
    let start = s.find('\'')? + 1;
    let end = s[start..].find('\'')? + start;
    Some(s[start..end].to_string())
}

/// Parse a mode id like `2560x1440@60.000` into `(2560, 1440)`; `None` if `s` isn't of
/// that shape (e.g. a connector/vendor/product/serial string encountered along the way).
fn parse_mode_id(s: &str) -> Option<(u32, u32)> {
    let (wh, _rate) = s.split_once('@')?;
    let (w, h) = wh.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Shipping mode: capture → ship dmabuf; recv input → inject. `transport` is already
/// connected (see `connect_retry`); `socket_path` is kept for logging.
async fn run_shipping(
    session: Session,
    transport: Arc<transport::Transport>,
    socket_path: &str,
    monitors: &[MonitorCfg],
    embedded: bool,
) -> Result<()> {
    // We rebuild the session on each live layout change, so own it mutably. `cursor_mode`
    // mirrors `main`: embedded composites the cursor, metadata ships it out-of-band.
    let mut session = session;
    let cursor_mode =
        if embedded { mutter::CURSOR_MODE_EMBEDDED } else { mutter::CURSOR_MODE_METADATA };

    let clone_id = std::env::var("RMNG_CLONE_ID")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| "clone".to_string());
    transport.send(&DaemonMsg::Hello(wire::socket::Hello { clone_id: clone_id.clone() }), &[])?;
    // Report the actual layout (configured positions, applied above) so the viewer routes
    // cross-window drags against it instead of assuming left-to-right.
    let layout: Vec<MonitorPlacement> = monitors
        .iter()
        .enumerate()
        .map(|(i, m)| MonitorPlacement { id: i as u32, x: m.x, y: m.y, width: m.w, height: m.h, primary: m.primary })
        .collect();
    transport.send(&DaemonMsg::Layout { monitors: layout }, &[])?;
    tracing::info!("connected to media socket {socket_path} as clone '{clone_id}'");

    // The session-bound runtime (rd + monitor_id→stream map) the input and clipboard tasks
    // read per event. A make-before-break swap re-points both at the NEW session by
    // mutating this one handle (see `reconfigure`).
    let active: ActiveSession = Arc::new(tokio::sync::Mutex::new(SessionRuntime {
        rd: session.rd.clone(),
        streams: session.monitors.iter().map(|m| (m.monitor_id, m.stream_path.clone())).collect(),
    }));
    // The live monitor set for the MCP task, refreshed by `reconfigure`. Task 3.5 points
    // `mcp::serve` at this; for now MCP still receives a startup snapshot below.
    let live_monitors: Arc<std::sync::Mutex<Vec<mutter::VirtualMonitor>>> =
        Arc::new(std::sync::Mutex::new(session.monitors.clone()));

    // Latest captured dmabuf per monitor, refreshed by the capture callbacks below; the
    // MCP `screenshot` tool dups the fd and GPU-encodes it to PNG.
    let latest: mcp::LatestFrames = Arc::new(std::sync::Mutex::new(HashMap::new()));

    // Input injection task: a blocking reader thread forwards ServerMsg::Input here. Each
    // event reads the CURRENT session from `active`, so input follows a live layout swap.
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<InputMsg>();
    {
        let active = active.clone();
        tokio::spawn(async move {
            while let Some(msg) = in_rx.recv().await {
                let rt = active.lock().await;
                let r = match msg {
                    InputMsg::PointerMove { monitor_id, x, y } => match rt.streams.get(&monitor_id) {
                        Some(s) => rt.rd.notify_pointer_motion_absolute(s, x, y).await,
                        None => Ok(()),
                    },
                    InputMsg::PointerRelative { dx, dy } => {
                        rt.rd.notify_pointer_motion_relative(dx, dy).await
                    }
                    InputMsg::Button { button, pressed } => {
                        rt.rd.notify_pointer_button(button, pressed).await
                    }
                    InputMsg::Axis { axis, step } => rt.rd.notify_pointer_axis_discrete(axis, step).await,
                    InputMsg::Key { keysym, pressed } => {
                        rt.rd.notify_keyboard_keysym(keysym, pressed).await
                    }
                    InputMsg::KeyCode { keycode, pressed } => {
                        rt.rd.notify_keyboard_keycode(keycode, pressed).await
                    }
                };
                if let Err(e) = r {
                    tracing::warn!("input inject failed: {e}");
                }
            }
        });
    }

    // Clipboard (rich + lazy): broker offers/requests/data drive the clone's selection. It
    // reads the CURRENT rd from `active` per op so it follows a swap (the old session's rd
    // is stopped afterwards).
    let (clip_tx, clip_rx) = tokio::sync::mpsc::unbounded_channel::<clipboard::FromServer>();
    tokio::spawn(clipboard::run(active.clone(), transport.clone(), clip_rx));

    // Per-monitor 1-deep flow control: ship a frame, then wait for its Ack before the next
    // (a slow receiver makes us drop frames here, not queue on the wire). Shared with the
    // reader thread (clears a gate on Ack) AND `reconfigure` (swaps the whole set), so Acks
    // reach the CURRENT session's gates after a swap.
    let in_flight: Arc<std::sync::Mutex<HashMap<u32, Arc<AtomicBool>>>> =
        Arc::new(std::sync::Mutex::new(
            session.monitors.iter().map(|m| (m.monitor_id, Arc::new(AtomicBool::new(false)))).collect(),
        ));

    // Reader thread: ServerMsg in (Input → inject; Ack → gate; ClipboardData → selection).
    {
        let (transport, flags) = (transport.clone(), in_flight.clone());
        std::thread::spawn(move || {
            loop {
                match transport.recv() {
                    Ok(ServerMsg::Input(im)) => {
                        let _ = in_tx.send(im);
                    }
                    Ok(ServerMsg::Ack(a)) => {
                        if let Some(f) = flags.lock().unwrap().get(&a.monitor_id) {
                            f.store(false, Ordering::Relaxed);
                        }
                    }
                    Ok(ServerMsg::ClipboardOffer(o)) => {
                        let _ = clip_tx.send(clipboard::FromServer::Offer(o));
                    }
                    Ok(ServerMsg::ClipboardRequest(r)) => {
                        let _ = clip_tx.send(clipboard::FromServer::Request(r));
                    }
                    Ok(ServerMsg::ClipboardData(d)) => {
                        let _ = clip_tx.send(clipboard::FromServer::Data(d));
                    }
                    Ok(_) => {} // Subscribe/FrameRequest — not used by the daemon
                    Err(e) => {
                        // The control-server went away (e.g. it restarted). Exit so systemd
                        // restarts us and we reconnect — capture/ship + input injection are
                        // useless without the socket, and the control loop otherwise blocks
                        // forever on `reconfig_rx.recv()` with a dead connection.
                        tracing::warn!("media socket closed ({e}); exiting to reconnect");
                        std::process::exit(1);
                    }
                }
            }
        });
    }

    // Capture → ship. Two backends (embedded GStreamer vs raw-PipeWire), chosen per
    // `embedded` by `spawn_capture_for`, which hands back a `CaptureHandle` we keep alive
    // in `capture` (and stop on swap). monitor_id → handle.
    let mut capture: HashMap<u32, CaptureHandle> = HashMap::new();
    for mon in &session.monitors {
        let gate = in_flight.lock().unwrap()[&mon.monitor_id].clone();
        capture.insert(
            mon.monitor_id,
            spawn_capture_for(mon, embedded, transport.clone(), latest.clone(), gate)?,
        );
    }

    // In production the viewer's input (server → daemon) drives frame damage, so the
    // cursor must NOT be auto-moved — it would fight the operator's pointer (yanking it
    // back to centre every 16 ms). Only nudge when explicitly testing without a viewer
    // (`RMNG_NUDGE=1`), which also exercises the METADATA cursor channel.
    if std::env::var_os("RMNG_NUDGE").is_some() {
        nudge_cursor(&session);
    }

    // Per-node computer-use MCP over HTTP: the in-clone agent connects directly and the
    // control-server's fleet MCP proxies to it. Shares the live Mutter session + frames.
    // (Task 3.5 re-points this at `live_monitors`; for now it takes a startup snapshot.)
    {
        let port: u16 =
            std::env::var("RMNG_DAEMON_MCP_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(9004);
        let (rd, conn, mons, latest_mcp, transport_mcp) =
            (session.rd.clone(), session.conn.clone(), session.monitors.clone(), latest.clone(), transport.clone());
        tokio::spawn(async move {
            if let Err(e) = mcp::serve(rd, conn, &mons, latest_mcp, transport_mcp, port).await {
                tracing::error!("clone-daemon MCP exited: {e:#}");
            }
        });
    }

    tracing::info!(
        "shipping {} monitor(s) ({}) …",
        session.monitors.len(),
        if embedded { "embedded cursor" } else { "client cursor / raw-PW" }
    );

    // Control loop: apply each pushed layout via a make-before-break session swap. The
    // sender side (`reconfig_tx`) is wired by Task 3.4 (the ServerMsg reader's Reconfigure
    // arm); `_reconfig_tx` is held here only to keep the channel open — so `recv()` blocks
    // (like the old terminal `pending()`) instead of returning None and exiting the daemon.
    let (_reconfig_tx, mut reconfig_rx) = tokio::sync::mpsc::channel::<Vec<MonitorCfg>>(4);
    let mut current_cfg = monitors.to_vec();
    while let Some(desired) = reconfig_rx.recv().await {
        if desired == current_cfg {
            continue; // idempotent (e.g. a Hello push matching the boot layout)
        }
        if let Err(e) = reconfigure(
            &mut session, &mut capture, &in_flight, &active, &live_monitors, &transport,
            &latest, cursor_mode, embedded, &desired,
        )
        .await
        {
            tracing::warn!("reconfigure failed: {e:#}");
            continue;
        }
        current_cfg = desired;
    }
    Ok(())
}

/// A running capture for one monitor, in whichever backend `embedded` selects. Held so it
/// stays alive; `stop_capture` tears it down on a session swap.
enum CaptureHandle {
    /// Embedded-cursor path: the GStreamer pipeline (drop / set-NULL to stop).
    Gst(gstreamer::Pipeline),
    /// Raw-PipeWire path: the capture thread's stop flag (set true to end `on_frame`).
    Pw(Arc<AtomicBool>),
}

/// Tear down one capture. Gst → NULL state (stops the pipeline); Pw → set the stop flag
/// (its thread also self-ends when the old session's node vanishes on `Session::stop`).
fn stop_capture(h: CaptureHandle) {
    use gstreamer::prelude::ElementExt;
    match h {
        CaptureHandle::Gst(p) => {
            let _ = p.set_state(gstreamer::State::Null);
        }
        CaptureHandle::Pw(flag) => flag.store(true, Ordering::Relaxed),
    }
}

/// Spawn the capture for one monitor on the backend `embedded` selects, gated by `gate`
/// (1-deep ack flow control). Used at startup and by `reconfigure` for the new session.
fn spawn_capture_for(
    mon: &mutter::VirtualMonitor,
    embedded: bool,
    transport: Arc<transport::Transport>,
    latest: mcp::LatestFrames,
    gate: Arc<AtomicBool>,
) -> Result<CaptureHandle> {
    if embedded {
        let (mid, (mw, mh)) = (mon.monitor_id, (mon.width, mon.height));
        let seq = Arc::new(AtomicU64::new(0));
        let p = capture::start_capture(mon, move |frame| {
            if gate.swap(true, Ordering::Relaxed) {
                return;
            }
            let n = seq.fetch_add(1, Ordering::Relaxed);
            ship_frame(&transport, mid, mw, mh, n, frame.fourcc, frame.modifier, frame.width, frame.height, &frame.planes, &frame.fds);
            store_latest(&latest, mid, frame.fourcc, frame.modifier, frame.width.min(mw), frame.height.min(mh), &frame.fds);
        })?;
        Ok(CaptureHandle::Gst(p))
    } else {
        Ok(CaptureHandle::Pw(spawn_pw_monitor(mon, transport, gate, latest)))
    }
}

/// Make-before-break session swap: build a fresh Mutter session with the full `desired`
/// monitor set, start capture on it, re-point input/clipboard at it, THEN stop the OLD
/// session (apps survive — Phase 0). Because the new outputs exist alongside the old until
/// the stop, gnome-shell never sees zero monitors, so windows don't collapse.
#[allow(clippy::too_many_arguments)]
async fn reconfigure(
    session: &mut mutter::Session,
    capture: &mut HashMap<u32, CaptureHandle>,
    in_flight: &Arc<std::sync::Mutex<HashMap<u32, Arc<AtomicBool>>>>,
    active: &ActiveSession,
    live_monitors: &Arc<std::sync::Mutex<Vec<mutter::VirtualMonitor>>>,
    transport: &Arc<transport::Transport>,
    latest: &mcp::LatestFrames,
    cursor_mode: u32,
    embedded: bool,
    desired: &[MonitorCfg],
) -> Result<()> {
    // 1. Build the NEW full session — its outputs appear ALONGSIDE the old
    //    (make-before-break). monitor_id == slot index.
    let sizes: Vec<(u32, u32)> = desired.iter().map(|m| (m.w, m.h)).collect();
    let new = mutter::setup_with_cursor_mode(&sizes, cursor_mode).await?;
    // Enable the clipboard on the new session so post-swap selection ops (set/read/write)
    // work; the clipboard task then uses this rd via the `active` handle updated below.
    if let Err(e) = new.rd.enable_clipboard(HashMap::new()).await {
        tracing::warn!("reconfigure: EnableClipboard on new session failed: {e}");
    }

    // 2. Start capture on the NEW nodes with fresh gates.
    let mut new_capture: HashMap<u32, CaptureHandle> = HashMap::new();
    let mut new_flags: HashMap<u32, Arc<AtomicBool>> = HashMap::new();
    for mon in &new.monitors {
        let gate = Arc::new(AtomicBool::new(false));
        new_flags.insert(mon.monitor_id, gate.clone());
        new_capture.insert(
            mon.monitor_id,
            spawn_capture_for(mon, embedded, transport.clone(), latest.clone(), gate)?,
        );
    }

    // 3. Point input + clipboard at the new session BEFORE dropping the old.
    {
        let mut rt = active.lock().await;
        rt.rd = new.rd.clone();
        rt.streams = new.monitors.iter().map(|m| (m.monitor_id, m.stream_path.clone())).collect();
    }

    // 4. Stop the OLD session (old outputs drop; gnome-shell reflows onto the new set),
    //    then position the new monitors. apply_layout AFTER stop → only the new connectors
    //    exist, so the connector match (Task 3.2) is unambiguous. (If on-device testing
    //    shows the new monitors need positioning before the old drop, move apply_layout
    //    before session.stop.)
    let _ = session.stop().await;
    apply_layout(desired).await;

    // 5. Stop old capture + release old gates (old nodes already gone; deterministic
    //    backstop). Replace the shared gate set with the new one so Acks now target it.
    for (_, h) in capture.drain() {
        stop_capture(h);
    }
    {
        let mut m = in_flight.lock().unwrap();
        for (_, f) in m.drain() {
            f.store(true, Ordering::Relaxed);
        }
        *m = new_flags;
    }

    // 6. Swap in the new session/capture + refresh the MCP monitor snapshot.
    *session = new;
    *capture = new_capture;
    *live_monitors.lock().unwrap() = session.monitors.clone();

    // 7. Report the applied layout (id == slot) so the viewer re-routes cross-window drags.
    let layout: Vec<MonitorPlacement> = desired
        .iter()
        .enumerate()
        .map(|(i, m)| MonitorPlacement { id: i as u32, x: m.x, y: m.y, width: m.w, height: m.h, primary: m.primary })
        .collect();
    transport.send(&DaemonMsg::Layout { monitors: layout }, &[])?;
    Ok(())
}

/// Ship one captured dmabuf frame as `DaemonMsg::Frame` + its fds (SCM_RIGHTS).
#[allow(clippy::too_many_arguments)]
fn ship_frame(
    transport: &transport::Transport,
    mid: u32,
    mw: u32,
    mh: u32,
    seq: u64,
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    planes: &[(u32, u32)],
    fds: &[std::os::fd::OwnedFd],
) {
    let msg = DaemonMsg::Frame(FrameMsg {
        monitor_id: mid,
        fourcc,
        modifier,
        width: width.min(mw),
        height: height.min(mh),
        planes: planes.iter().map(|&(offset, stride)| PlaneLayout { offset, stride }).collect(),
        seq,
    });
    let raw: Vec<i32> = fds.iter().map(|f| f.as_raw_fd()).collect();
    if let Err(e) = transport.send(&msg, &raw) {
        tracing::warn!("ship frame failed: {e}");
    }
    // `fds` (OwnedFd) are dropped by the caller → closes our copies; the kernel
    // dup'd them into the socket via SCM_RIGHTS.
}

/// Remember the latest captured dmabuf for a monitor (dup the first plane's fd) so the
/// MCP `screenshot` tool can GPU-encode it on demand. Replaces (and closes) the prior fd.
fn store_latest(latest: &mcp::LatestFrames, mid: u32, fourcc: u32, modifier: u64, w: u32, h: u32, fds: &[OwnedFd]) {
    let Some(fd0) = fds.first() else { return };
    let Ok(raw) = nix::unistd::dup(fd0.as_raw_fd()) else { return };
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    latest.lock().unwrap().insert(mid, mcp::LatestFrame { fd, fourcc, modifier, width: w, height: h });
}

/// Spawn the raw-PipeWire capture for one monitor on its own thread (the pw
/// mainloop is blocking + `!Send`). Ships frames (ack-gated) + cursor metadata.
/// Returns a stop flag: set it true to make `on_frame` early-return on a session swap
/// (belt-and-suspenders — the capture also self-ends when the old node vanishes on
/// `Session::stop`).
fn spawn_pw_monitor(
    mon: &mutter::VirtualMonitor,
    transport: Arc<transport::Transport>,
    gate: Arc<AtomicBool>,
    latest: mcp::LatestFrames,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let node_id = mon.node_id;
    let mid = mon.monitor_id;
    let (mw, mh) = (mon.width, mon.height);
    let ship = transport.clone();
    let cursor_tx = transport;
    std::thread::Builder::new()
        .name(format!("pw-capture-{mid}"))
        .spawn(move || {
            let mut seq = 0u64;
            let on_frame = move |frame: capture_pw::PwFrame| {
                // Stop shipping once this capture is being torn down (session swap).
                if stop_thread.load(Ordering::Relaxed) {
                    return;
                }
                // Drop this frame if the previous one isn't acked yet (back-pressure).
                if gate.swap(true, Ordering::Relaxed) {
                    return;
                }
                seq += 1;
                if seq == 1 {
                    tracing::debug!("pw monitor {mid}: first frame {}x{} fourcc={:#010x} modifier={:#018x}", frame.width, frame.height, frame.fourcc, frame.modifier);
                }
                ship_frame(&ship, mid, mw, mh, seq, frame.fourcc, frame.modifier, frame.width, frame.height, &frame.planes, &frame.fds);
                store_latest(&latest, mid, frame.fourcc, frame.modifier, frame.width.min(mw), frame.height.min(mh), &frame.fds);
            };
            let on_cursor = move |c: capture_pw::PwCursor| {
                let shape = c.shape.map(|(width, height, hotspot_x, hotspot_y, rgba)| CursorShape {
                    width,
                    height,
                    hotspot_x,
                    hotspot_y,
                    rgba, // BGRA8888 premultiplied (SPA cursor); the viewer uses that memory format
                });
                // Passive capture echo (the user's own / app cursor); not a warp.
                let _ = cursor_tx.send(&DaemonMsg::Cursor(CursorMeta { monitor_id: mid, x: c.x, y: c.y, shape, warp: false }), &[]);
            };
            if let Err(e) = capture_pw::run(node_id, on_frame, on_cursor) {
                tracing::error!("raw-pw capture for monitor {mid} exited: {e:#}");
            }
        })
        .expect("spawn pw-capture thread");
    stop
}

/// Standalone capture self-test: log first frame + fps, nudge cursor for damage.
async fn run_capture_test(session: Session) -> Result<()> {
    let mut pipelines = Vec::new();
    let mut counters: Vec<(u32, Arc<AtomicU64>)> = Vec::new();
    for mon in &session.monitors {
        let counter = Arc::new(AtomicU64::new(0));
        let first = Arc::new(AtomicBool::new(true));
        let (c, f, mid) = (counter.clone(), first.clone(), mon.monitor_id);
        let pipeline = capture::start_capture(mon, move |frame| {
            c.fetch_add(1, Ordering::Relaxed);
            if f.swap(false, Ordering::Relaxed) {
                tracing::info!(
                    "monitor {} first frame: fourcc={:#010x} modifier={:#018x} {}x{} planes={:?} fds={}",
                    mid, frame.fourcc, frame.modifier, frame.width, frame.height, frame.planes, frame.fds.len()
                );
            }
        })?;
        pipelines.push(pipeline);
        counters.push((mon.monitor_id, counter));
    }
    nudge_cursor(&session);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            for (id, c) in &counters {
                tracing::info!("monitor {id} capture fps: {}", c.swap(0, Ordering::Relaxed));
            }
        }
    });
    tracing::info!("capturing (Ctrl-C to stop) …");
    futures::future::pending::<()>().await;
    Ok(())
}

/// Oscillate the pointer on **every** monitor so the damage-driven capture emits
/// frames (test-only; in production the viewer/agent input creates the damage).
fn nudge_cursor(session: &Session) {
    for m in &session.monitors {
        let rd = session.rd.clone();
        let stream = m.stream_path.clone();
        let (w, h) = (m.width as f64, m.height as f64);
        tokio::spawn(async move {
            let mut t = 0u32;
            loop {
                t = t.wrapping_add(1);
                let x = w / 2.0 + if t % 2 == 0 { 20.0 } else { -20.0 };
                let _ = rd.notify_pointer_motion_absolute(&stream, x, h / 2.0).await;
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connectors_and_current_modes() {
        // Real GetCurrentState shape from Phase 0 (CT 113, GNOME 48), scales trimmed.
        let blob = "(uint32 3, [(('Meta-0', 'MetaVendor', 'Virtual remote monitor', '0x000001'), \
[('2560x1440@60.000', 2560, 1440, 60.0, 1.0, [1.0, 1.25], {'is-current': <true>, 'is-preferred': <true>})], \
{'is-builtin': <false>}), (('Meta-1', 'MetaVendor', 'Virtual remote monitor', '0x000002'), \
[('1920x1080@60.000', 1920, 1080, 60.0, 1.0, [1.0], {'is-current': <true>, 'is-preferred': <true>})], \
{'is-builtin': <false>})], [(2560, 0, 1.0, uint32 0, true, [('Meta-0', 'x', 'y', '0x1')], @a{sv} {}), \
(0, 0, 1.0, 0, false, [('Meta-1', 'x', 'y', '0x2')], {})], {'layout-mode': <uint32 1>})";
        let conns = parse_connectors(blob);
        assert_eq!(conns.len(), 2);
        assert_eq!(conns[0], ("Meta-0".to_string(), 2560, 1440));
        assert_eq!(conns[1], ("Meta-1".to_string(), 1920, 1080));
    }

    #[test]
    fn parse_connectors_picks_is_current_mode() {
        // Monitor with two modes: first (3840x2160) is NOT current, second (1920x1080) IS current.
        // Ensures parser selects the is-current mode rather than blindly taking the first mode.
        let blob = "(uint32 5, [(('Meta-0', 'MetaVendor', 'Virtual remote monitor', '0x000001'), \
[('3840x2160@60.000', 3840, 2160, 60.0, 1.0, [1.0], {'is-preferred': <true>}), \
('1920x1080@60.000', 1920, 1080, 60.0, 1.0, [1.0], {'is-current': <true>})], \
{'is-builtin': <false>})], [(0, 0, 1.0, uint32 0, true, [('Meta-0', 'x', 'y', '0x1')], @a{sv} {})], {'layout-mode': <uint32 1>})";
        let conns = parse_connectors(blob);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0], ("Meta-0".to_string(), 1920, 1080));
    }
}
