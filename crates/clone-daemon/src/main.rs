//! `clone-daemon` (Phase 3) — the thin in-clone capture + input pipe.
//!
//! Three run modes:
//!   - `--session-holder` → **holder**: own the clone's Mutter sessions, virtual monitors,
//!     input injection and clipboard, and hold them across daemon restarts (see [`holder`]).
//!   - `RMNG_SOCKET=<path>` → **shipping**: connect to control-server's media
//!     socket, ship each monitor's dmabuf (FrameMsg + fds via SCM_RIGHTS), and
//!     relay incoming input to the holder.
//!   - otherwise → **capture self-test**: log fourcc/modifier/size + fps (cursor
//!     nudge generates damage on the static headless desktop).

mod capture;
mod capture_pw;
mod clipboard;
mod holder;
mod ipc;
mod keysym;
mod mcp;
mod mutter;
mod session;
mod transport;
mod windows;

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use wire::holder::{FromHolder, HolderMonitor, ToHolder};
use wire::socket::{
    CursorMeta, CursorShape, DaemonMsg, FrameMsg, MonitorPlacement, PlaneLayout, ServerMsg,
};

use crate::mutter::Session;
use crate::session::Holder;

/// One configured monitor: size, position (unified-desktop px) + primary flag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MonitorCfg {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) primary: bool,
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
            Some(MonitorCfg {
                w,
                h,
                x,
                y,
                primary,
            })
        })
        .collect();
    if mons.is_empty() {
        mons.push(MonitorCfg {
            w: 1920,
            h: 1080,
            x: 0,
            y: 0,
            primary: true,
        });
    }
    if !mons.iter().any(|m| m.primary) {
        mons[0].primary = true;
    }
    mons
}

/// The layout the server pushed, as monitor slots. Guarantees at least one monitor and
/// exactly one primary, the same two invariants [`parse_monitors`] enforces.
pub(crate) fn monitors_from_specs(specs: &[wire::control::MonitorSpec]) -> Vec<MonitorCfg> {
    let mut mons: Vec<MonitorCfg> = specs
        .iter()
        .map(|m| MonitorCfg {
            w: m.width,
            h: m.height,
            x: m.x as i32,
            y: m.y as i32,
            primary: m.primary,
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

    gstreamer::init()?;
    let monitors = parse_monitors(std::env::var("RMNG_MONITORS").ok());
    let sizes: Vec<(u32, u32)> = monitors.iter().map(|m| (m.w, m.h)).collect();
    let socket = std::env::var("RMNG_SOCKET").ok();
    let holder_mode = std::env::args().any(|a| a == "--session-holder");

    // Client-drawn cursor is the default for shipping: capture in cursor-mode
    // METADATA (cursor out-of-band via SPA_META_Cursor, read by the raw-PW path)
    // so the viewer draws it locally. RMNG_EMBEDDED_CURSOR=1 forces the
    // GStreamer/embedded-cursor path (cursor composited into the frame). The
    // standalone capture self-test always uses embedded (GStreamer).
    //
    // `!holder_mode` on that last clause is load-bearing. The holder has no `RMNG_SOCKET`
    // either, its unit carrying only `WAYLAND_DISPLAY`, so reading a missing socket as "this
    // is the self-test" made every session it built composite the cursor into the frame. The
    // viewer then showed a painted-on pointer next to the real one and had no shape to apply
    // to its own. Set `RMNG_EMBEDDED_CURSOR` on BOTH units to force embedding: the holder
    // picks the session's cursor mode and the daemon picks the matching capture path.
    let embedded =
        std::env::var("RMNG_EMBEDDED_CURSOR").is_ok() || (socket.is_none() && !holder_mode);
    let cursor_mode = if embedded {
        mutter::CURSOR_MODE_EMBEDDED
    } else {
        mutter::CURSOR_MODE_METADATA
    };
    if holder_mode {
        tracing::info!(?sizes, "clone-daemon: session-holder mode");
        return holder::run(monitors, cursor_mode).await;
    }
    match socket {
        Some(path) => {
            // Connect to the media socket FIRST (retrying while it's unavailable), THEN
            // reach the holder — so a down/restarting control-server costs nothing but this
            // cheap retry loop. On a later disconnect we exit and systemd restarts us back
            // into it.
            let transport = connect_retry(&path).await;
            let holder = Holder::connect().await?;
            run_shipping(holder, transport, &path, embedded).await
        }
        None => {
            tracing::info!(?sizes, embedded, "clone-daemon: setting up Mutter session");
            let session = mutter::setup_with_cursor_mode(&sizes, cursor_mode).await?;
            tracing::info!(
                "session ready: {} virtual monitor(s)",
                session.monitors.len()
            );
            run_capture_test(session).await
        }
    }
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
                    tracing::warn!(
                        "media socket {socket_path} unavailable ({e}); retrying every 1s"
                    );
                    warned = true;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}


/// Shipping mode: capture the holder's monitors, ship dmabufs, relay input and clipboard.
///
/// Owns nothing session-bound. The monitor set arrives from the holder and changes under us
/// when a layout is pushed or Mutter closes the session, so capture is (re)built from
/// whatever generation the holder last announced.
async fn run_shipping(
    holder: Holder,
    transport: Arc<transport::Transport>,
    socket_path: &str,
    embedded: bool,
) -> Result<()> {
    let holder = Arc::new(holder);
    let clone_id = std::env::var("RMNG_CLONE_ID")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "clone".to_string());
    transport.send(
        &DaemonMsg::Hello(wire::socket::Hello {
            clone_id: clone_id.clone(),
            fresh_session: holder.fresh_session(),
        }),
        &[],
    )?;
    tracing::info!("connected to media socket {socket_path} as clone '{clone_id}'");

    // Latest captured dmabuf per monitor, refreshed by the capture callbacks below; the
    // MCP `screenshot` tool dups the fd and GPU-encodes it to PNG.
    let latest: mcp::LatestFrames = Arc::new(std::sync::Mutex::new(HashMap::new()));
    // The live monitor set for the MCP task, refreshed on every generation. `mcp::serve`
    // reads this per request so screenshots and geometry follow a layout swap.
    let live_monitors: Arc<std::sync::Mutex<Vec<HolderMonitor>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Per-monitor 1-deep flow control: ship a frame, then wait for its Ack before the next
    // (a slow receiver makes us drop frames here, not queue on the wire). Shared with the
    // reader thread, which clears a gate on Ack.
    let in_flight: Arc<std::sync::Mutex<HashMap<u32, Arc<AtomicBool>>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    // Capture on/off requests, from the server (a viewer started or stopped watching this
    // clone) and from the MCP screenshot path (wake a stopped capture for one frame). The
    // control loop below owns the capture threads and is the only place that acts on these.
    let (capture_tx, mut capture_rx) = tokio::sync::mpsc::unbounded_channel::<mcp::CaptureCmd>();
    let capture_ctl = mcp::CaptureCtl::new(capture_tx);

    // Reader thread, control-server side: Input and clipboard go straight to the holder,
    // Ack clears a gate, SetMonitors becomes a layout the holder applies.
    {
        let (transport, flags, holder) = (transport.clone(), in_flight.clone(), holder.clone());
        let capture_ctl = capture_ctl.clone();
        std::thread::spawn(move || {
            loop {
                match transport.recv() {
                    Ok(ServerMsg::Input(im)) => holder.send(ToHolder::Input(im)),
                    Ok(ServerMsg::Ack(a)) => {
                        if let Some(f) = flags.lock().unwrap().get(&a.monitor_id) {
                            f.store(false, Ordering::Relaxed);
                        }
                    }
                    Ok(ServerMsg::ClipboardOffer(o)) => holder.send(ToHolder::ClipboardOffer(o)),
                    Ok(ServerMsg::ClipboardRequest(r)) => {
                        holder.send(ToHolder::ClipboardRequest(r))
                    }
                    Ok(ServerMsg::ClipboardData(d)) => holder.send(ToHolder::ClipboardData(d)),
                    Ok(ServerMsg::SetMonitors { monitors }) => {
                        holder.send(ToHolder::SetLayout { monitors })
                    }
                    Ok(ServerMsg::Capture { active }) => capture_ctl.set_wanted(active),
                    Ok(_) => {} // Subscribe/FrameRequest — not used by the daemon
                    Err(e) => {
                        // The control-server went away (e.g. it restarted). Exit so systemd
                        // restarts us and we reconnect — capture and shipping are useless
                        // without the socket. The holder keeps the monitors open meanwhile,
                        // so this costs no window positions.
                        tracing::warn!("media socket closed ({e}); exiting to reconnect");
                        std::process::exit(1);
                    }
                }
            }
        });
    }

    // Per-node computer-use MCP over HTTP: the in-clone agent connects directly and the
    // control-server's fleet MCP proxies to it. Reads the live monitor set per request, so
    // screenshots and input follow a layout swap.
    {
        let port: u16 = std::env::var("RMNG_DAEMON_MCP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(9004);
        // Height of the MCP's virtual coordinate space: screenshots are downscaled to it and
        // pointer x/y are read in it (see `mcp`'s module docs). 1080 keeps images and coords
        // in the range vision models are trained on; `0` serves everything at native res.
        let virt_height: u32 = std::env::var("RMNG_DESKTOP_HEIGHT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1080);
        let (holder_mcp, live_mons_mcp, latest_mcp, transport_mcp, ctl_mcp) = (
            holder.clone(),
            live_monitors.clone(),
            latest.clone(),
            transport.clone(),
            capture_ctl.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = mcp::serve(
                holder_mcp,
                live_mons_mcp,
                latest_mcp,
                transport_mcp,
                port,
                virt_height,
                ctl_mcp,
            )
            .await
            {
                tracing::error!("clone-daemon MCP exited: {e:#}");
            }
        });
    }

    tracing::info!(
        "shipping from the session holder ({}) …",
        if embedded { "embedded cursor" } else { "client cursor / raw-PW" }
    );

    // Control loop. Two things drive it: the holder announcing a generation of monitors, and
    // capture going on or off. Each generation gets its own capture set, and the previous one
    // keeps running until the holder confirms its monitors are gone, so no frame gap opens in
    // the middle of a swap.
    //
    // Capture runs only while somebody is watching. `wanted` is the server's latest answer
    // and starts true, so a server too old to send `Capture` leaves the previous behaviour
    // intact. `wake_until` is a screenshot's temporary override (see [`WAKE_HOLD`]).
    let mut capture: HashMap<u32, CaptureHandle> = HashMap::new();
    let mut retiring: Vec<CaptureHandle> = Vec::new();
    let mut generation = 0u64;
    let mut monitors: Vec<HolderMonitor> = Vec::new();
    let mut wanted = true;
    let mut wake_until: Option<tokio::time::Instant> = None;
    let mut inbox = holder.inbox();
    loop {
        let deadline = wake_until;
        let msg = tokio::select! {
            msg = inbox.recv() => match msg {
                Some(m) => m,
                // The holder connection ended. Exit so systemd restarts us into the
                // reconnect path.
                None => anyhow::bail!("the session holder disconnected"),
            },
            cmd = capture_rx.recv() => {
                match cmd {
                    Some(mcp::CaptureCmd::SetWanted(active)) => wanted = active,
                    Some(mcp::CaptureCmd::Wake) => {
                        wake_until = Some(tokio::time::Instant::now() + WAKE_HOLD);
                    }
                    None => continue,
                }
                let run = wanted || wake_until.is_some();
                if run && capture.is_empty() && !monitors.is_empty() {
                    match start_captures(&monitors, embedded, &transport, &latest, &in_flight) {
                        Ok(started) => {
                            capture = started;
                            capture_ctl.set_running(true);
                            tracing::info!("capture resumed on {} monitor(s)", capture.len());
                        }
                        Err(e) => tracing::error!("capture failed to resume: {e:#}"),
                    }
                } else if !run && !capture.is_empty() {
                    stop_all(&mut capture, &in_flight);
                    capture_ctl.set_running(false);
                    tracing::info!("capture stopped (nobody is watching this clone)");
                }
                continue;
            }
            () = sleep_until_opt(deadline) => {
                // The screenshot's grace period is over. Stop again unless a viewer arrived
                // while it was open.
                wake_until = None;
                if !wanted && !capture.is_empty() {
                    stop_all(&mut capture, &in_flight);
                    capture_ctl.set_running(false);
                    tracing::debug!("capture stopped again after the screenshot wake");
                }
                continue;
            }
        };
        let (announced, announced_monitors) = match msg {
            FromHolder::HelloOk { generation, monitors, .. } => (generation, monitors),
            FromHolder::Monitors { generation, monitors } => (generation, monitors),
            FromHolder::SwapDone { generation: done } => {
                // The old monitors are gone, so their capture threads have nothing left to
                // read. Anything newer than what we are capturing is not ours to act on.
                if done == generation {
                    for h in retiring.drain(..) {
                        stop_capture(h);
                    }
                }
                continue;
            }
            FromHolder::ClipboardOffer(o) => {
                let _ = transport.send(&DaemonMsg::ClipboardOffer(o), &[]);
                continue;
            }
            FromHolder::ClipboardRequest(r) => {
                let _ = transport.send(&DaemonMsg::ClipboardRequest(r), &[]);
                continue;
            }
            FromHolder::ClipboardData(d) => {
                let _ = transport.send(&DaemonMsg::ClipboardData(d), &[]);
                continue;
            }
            FromHolder::Unknown => continue,
        };
        if announced <= generation {
            continue; // a generation we already captured
        }
        generation = announced;
        monitors = announced_monitors;

        // Start capture on the new nodes with fresh gates, then publish both in one breath.
        //
        // Ack routing keys off `in_flight`: the new capture ships frames keyed by the NEW
        // monitor_ids, and each frame's Ack must find that monitor's gate there to clear its
        // 1-deep gate. Publishing late would leave a grown monitor's id absent from the map,
        // its Ack dropped, and that monitor frozen after one frame.
        let mut new_capture = HashMap::new();
        if wanted || wake_until.is_some() {
            match start_captures(&monitors, embedded, &transport, &latest, &in_flight) {
                Ok(started) => new_capture = started,
                Err(e) => {
                    tracing::error!("capture failed to start for generation {announced}: {e:#}");
                    continue;
                }
            }
        } else {
            // Not capturing this generation, so no monitor has a gate. Clearing them keeps a
            // stale Ack from clearing a gate that a later resume will hand to a new thread.
            in_flight.lock().unwrap().clear();
        }
        capture_ctl.set_running(!new_capture.is_empty());
        *live_monitors.lock().unwrap() = monitors.clone();
        // Hold the previous captures until the holder says the old monitors are gone.
        retiring.extend(std::mem::take(&mut capture).into_values());
        capture = new_capture;
        {
            let live: std::collections::HashSet<u32> =
                monitors.iter().map(|m| m.monitor_id).collect();
            latest.lock().unwrap().retain(|id, _| live.contains(id));
        }
        // Always acknowledge the generation, capturing or not: the holder's make-before-break
        // swap waits on this before it drops the old session, and a clone nobody is watching
        // still has to finish a layout change.
        holder.send(ToHolder::CaptureReady { generation: announced });
        // Report the applied layout so the viewer routes cross-window drags against it.
        let layout: Vec<MonitorPlacement> = monitors.iter().map(|m| m.placement()).collect();
        transport.send(&DaemonMsg::Layout { monitors: layout }, &[])?;
        tracing::info!(
            "{} {} monitor(s), generation {announced}",
            if capture.is_empty() { "holding" } else { "capturing" },
            monitors.len()
        );
    }
}

/// How long a screenshot's wake keeps capture alive. An agent driving the desktop shoots
/// repeatedly (every pointer tool returns a settle image), so tearing capture down between
/// two calls a second apart would pay the restart cost on each one.
const WAKE_HOLD: Duration = Duration::from_secs(5);

/// `sleep_until` for an optional deadline: with `None` this never completes, which is what a
/// `select!` arm needs when there is no wake pending.
async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// Start capture for every monitor and publish their 1-deep gates. On failure nothing is
/// left running: a half-started set would ship some monitors and freeze others.
fn start_captures(
    monitors: &[HolderMonitor],
    embedded: bool,
    transport: &Arc<transport::Transport>,
    latest: &mcp::LatestFrames,
    in_flight: &Arc<std::sync::Mutex<HashMap<u32, Arc<AtomicBool>>>>,
) -> Result<HashMap<u32, CaptureHandle>> {
    let mut started: HashMap<u32, CaptureHandle> = HashMap::new();
    let mut gates: HashMap<u32, Arc<AtomicBool>> = HashMap::new();
    for mon in monitors {
        let gate = Arc::new(AtomicBool::new(false));
        gates.insert(mon.monitor_id, gate.clone());
        match spawn_capture_for(mon, embedded, transport.clone(), latest.clone(), gate) {
            Ok(h) => {
                started.insert(mon.monitor_id, h);
            }
            Err(e) => {
                for (_, h) in started.drain() {
                    stop_capture(h);
                }
                return Err(e);
            }
        }
    }
    let mut live = in_flight.lock().unwrap();
    for (_, f) in live.drain() {
        f.store(true, Ordering::Relaxed); // close old gates → old capture stops shipping
    }
    *live = gates;
    Ok(started)
}

/// Stop every running capture and drop the gates with them.
fn stop_all(
    capture: &mut HashMap<u32, CaptureHandle>,
    in_flight: &Arc<std::sync::Mutex<HashMap<u32, Arc<AtomicBool>>>>,
) {
    for (_, h) in capture.drain() {
        stop_capture(h);
    }
    in_flight.lock().unwrap().clear();
}

/// A running capture for one monitor, in whichever backend `embedded` selects. Held so it
/// stays alive; `stop_capture` tears it down on a session swap or when the server says
/// nobody is watching this clone.
enum CaptureHandle {
    /// Embedded-cursor path: the GStreamer pipeline (drop / set-NULL to stop).
    Gst(gstreamer::Pipeline),
    /// Raw-PipeWire path: the capture thread's shipping stop flag, plus the channel that
    /// ends its mainloop and so disconnects from the node.
    Pw(Arc<AtomicBool>, pipewire::channel::Sender<()>),
}

/// Tear down one capture, all the way to disconnecting from the node.
///
/// Both halves matter for the Pw arm. The flag stops frames reaching the socket at once,
/// and the channel ends the mainloop so the stream disconnects — a consumer that stays
/// connected keeps Mutter painting the monitor whether or not we read the frames.
fn stop_capture(h: CaptureHandle) {
    use gstreamer::prelude::ElementExt;
    match h {
        CaptureHandle::Gst(p) => {
            let _ = p.set_state(gstreamer::State::Null);
        }
        CaptureHandle::Pw(flag, quit) => {
            flag.store(true, Ordering::Relaxed);
            let _ = quit.send(());
        }
    }
}

/// Spawn the capture for one monitor on the backend `embedded` selects, gated by `gate`
/// (1-deep ack flow control). Used at startup and by `reconfigure` for the new session.
fn spawn_capture_for(
    mon: &HolderMonitor,
    embedded: bool,
    transport: Arc<transport::Transport>,
    latest: mcp::LatestFrames,
    gate: Arc<AtomicBool>,
) -> Result<CaptureHandle> {
    if embedded {
        let (mid, (mw, mh)) = (mon.monitor_id, (mon.width, mon.height));
        let seq = Arc::new(AtomicU64::new(0));
        let p = capture::start_capture(mon.node_id, move |frame| {
            if gate.swap(true, Ordering::Relaxed) {
                return;
            }
            let n = seq.fetch_add(1, Ordering::Relaxed);
            ship_frame(
                &transport,
                mid,
                mw,
                mh,
                n,
                frame.fourcc,
                frame.modifier,
                frame.width,
                frame.height,
                &frame.planes,
                &frame.fds,
                // The GStreamer backend pins DMABuf caps, so its frames are never shm.
                false,
            );
            store_latest(
                &latest,
                mid,
                frame.fourcc,
                frame.modifier,
                frame.width.min(mw),
                frame.height.min(mh),
                &frame.planes,
                &frame.fds,
                false,
            );
        })?;
        Ok(CaptureHandle::Gst(p))
    } else {
        let (stop, quit) = spawn_pw_monitor(mon, transport, gate, latest);
        Ok(CaptureHandle::Pw(stop, quit))
    }
}

/// Ship one captured frame as `DaemonMsg::Frame` + its fds (SCM_RIGHTS). `shm` marks the
/// fds as memfds of system memory rather than dmabufs (a clone with no GPU).
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
    shm: bool,
) {
    let msg = DaemonMsg::Frame(FrameMsg {
        monitor_id: mid,
        fourcc,
        modifier,
        width: width.min(mw),
        height: height.min(mh),
        planes: planes
            .iter()
            .map(|&(offset, stride)| PlaneLayout { offset, stride })
            .collect(),
        seq,
        shm,
    });
    let raw: Vec<i32> = fds.iter().map(|f| f.as_raw_fd()).collect();
    if let Err(e) = transport.send(&msg, &raw) {
        tracing::warn!("ship frame failed: {e}");
    }
    // `fds` (OwnedFd) are dropped by the caller → closes our copies; the kernel
    // dup'd them into the socket via SCM_RIGHTS.
}

/// Remember the latest captured frame for a monitor (dup the first plane's fd) so the
/// MCP `screenshot` tool can encode it on demand. Replaces (and closes) the prior fd.
/// `shm` picks the CPU encode path in `mcp`, since a GPU-less clone has no VA-API either.
#[allow(clippy::too_many_arguments)]
fn store_latest(
    latest: &mcp::LatestFrames,
    mid: u32,
    fourcc: u32,
    modifier: u64,
    w: u32,
    h: u32,
    planes: &[(u32, u32)],
    fds: &[OwnedFd],
    shm: bool,
) {
    let Some(fd0) = fds.first() else { return };
    let Ok(raw) = nix::unistd::dup(fd0.as_raw_fd()) else {
        return;
    };
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let planes = planes
        .iter()
        .map(|&(offset, stride)| PlaneLayout { offset, stride })
        .collect();
    latest.lock().unwrap().insert(
        mid,
        mcp::LatestFrame {
            fd,
            fourcc,
            modifier,
            width: w,
            height: h,
            planes,
            shm,
            stamp: mcp::next_stamp(),
        },
    );
}

/// Spawn the raw-PipeWire capture for one monitor on its own thread (the pw
/// mainloop is blocking + `!Send`). Ships frames (ack-gated) + cursor metadata.
///
/// Returns the shipping stop flag plus a channel that ends the mainloop. The flag stops
/// frames reaching the socket; only the channel disconnects from the node, which is what
/// makes Mutter stop painting.
fn spawn_pw_monitor(
    mon: &HolderMonitor,
    transport: Arc<transport::Transport>,
    gate: Arc<AtomicBool>,
    latest: mcp::LatestFrames,
) -> (Arc<AtomicBool>, pipewire::channel::Sender<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let (quit_tx, quit_rx) = pipewire::channel::channel::<()>();
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
                    tracing::debug!(
                        "pw monitor {mid}: first frame {}x{} fourcc={:#010x} modifier={:#018x}",
                        frame.width,
                        frame.height,
                        frame.fourcc,
                        frame.modifier
                    );
                }
                ship_frame(
                    &ship,
                    mid,
                    mw,
                    mh,
                    seq,
                    frame.fourcc,
                    frame.modifier,
                    frame.width,
                    frame.height,
                    &frame.planes,
                    &frame.fds,
                    frame.shm,
                );
                store_latest(
                    &latest,
                    mid,
                    frame.fourcc,
                    frame.modifier,
                    frame.width.min(mw),
                    frame.height.min(mh),
                    &frame.planes,
                    &frame.fds,
                    frame.shm,
                );
            };
            let on_cursor = move |c: capture_pw::PwCursor| {
                let shape =
                    c.shape
                        .map(|(width, height, hotspot_x, hotspot_y, rgba)| CursorShape {
                            width,
                            height,
                            hotspot_x,
                            hotspot_y,
                            rgba, // BGRA8888 premultiplied (SPA cursor); the viewer uses that memory format
                        });
                // Passive capture echo (the user's own / app cursor); not a warp.
                let hidden = c.hidden == Some(true);
                let _ = cursor_tx.send(
                    &DaemonMsg::Cursor(CursorMeta {
                        monitor_id: mid,
                        x: c.x,
                        y: c.y,
                        shape,
                        warp: false,
                        hidden,
                    }),
                    &[],
                );
            };
            if let Err(e) = capture_pw::run(node_id, on_frame, on_cursor, quit_rx) {
                tracing::error!("raw-pw capture for monitor {mid} exited: {e:#}");
            }
        })
        .expect("spawn pw-capture thread");
    (stop, quit_tx)
}

/// Standalone capture self-test: log first frame + fps, nudge cursor for damage.
async fn run_capture_test(session: Session) -> Result<()> {
    let mut pipelines = Vec::new();
    let mut counters: Vec<(u32, Arc<AtomicU64>)> = Vec::new();
    for mon in &session.monitors {
        let counter = Arc::new(AtomicU64::new(0));
        let first = Arc::new(AtomicBool::new(true));
        let (c, f, mid) = (counter.clone(), first.clone(), mon.monitor_id);
        let pipeline = capture::start_capture(mon.node_id, move |frame| {
            c.fetch_add(1, Ordering::Relaxed);
            if f.swap(false, Ordering::Relaxed) {
                tracing::info!(
                    "monitor {} first frame: fourcc={:#010x} modifier={:#018x} {}x{} planes={:?} fds={}",
                    mid,
                    frame.fourcc,
                    frame.modifier,
                    frame.width,
                    frame.height,
                    frame.planes,
                    frame.fds.len()
                );
            }
        })?;
        pipelines.push(pipeline);
        counters.push((mon.monitor_id, counter));
    }
    holder::nudge_cursor(&session);
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
