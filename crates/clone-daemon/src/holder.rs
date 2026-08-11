//! Session-holder mode (`rmng-clone-daemon --session-holder`).
//!
//! Holds the clone's Mutter sessions and virtual monitors open across daemon restarts.
//! Mutter destroys a RemoteDesktop session when the D-Bus connection that created it drops,
//! and gnome-shell remaps every window when the monitor set empties, so a daemon that owned
//! the session reset every window position on each payload push. This process owns it
//! instead, and only restarts when the protocol between the two changes.
//!
//! It owns everything Mutter will answer for no other connection: the sessions, the virtual
//! monitors, `apply_layout`, input injection, and the clipboard bridge. The daemon keeps
//! capture, encode, shipping, and the MCP, and reconnects here after each of its restarts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use wire::holder::{FromHolder, HolderMonitor, PROTO_VERSION, ToHolder};
use wire::socket::InputMsg;

use crate::mutter::{self, Session};
use crate::{MonitorCfg, clipboard, ipc, monitors_from_specs};

/// The session-bound state the input and clipboard tasks read, swapped atomically by
/// [`swap`]. `rd` injects input, `streams` maps monitor_id to the stream path absolute
/// pointer motion needs, and `conn` is the session bus the session lives on.
///
/// Each task reads this per operation rather than holding a copy, so a make-before-break
/// swap re-points them all by mutating this one handle. Replacing `conn` here does NOT close
/// the old connection (the clipboard signal tasks hold proxy clones of it while parked in
/// `sig.next()`), so [`swap`] closes it explicitly to end those streams and trigger their
/// re-subscribe.
pub(crate) struct SessionRuntime {
    pub(crate) rd: mutter::RemoteDesktopSessionProxy<'static>,
    pub(crate) conn: zbus::Connection,
    pub(crate) streams: HashMap<u32, String>,
}
pub(crate) type ActiveSession = Arc<tokio::sync::Mutex<SessionRuntime>>;

/// Queue to whichever daemon is connected. A message queued while none is drops, which is
/// correct: a daemon that reconnects asks for the current state in its `Hello`.
pub(crate) type Out = UnboundedSender<FromHolder>;

/// How long a swap waits for the daemon to confirm capture on the new monitors.
///
/// Only reached if the daemon died between the two messages. Finishing the swap anyway beats
/// leaving both sessions alive, since the old one's monitors are what a fresh daemon would
/// then have to reconcile against.
const CAPTURE_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the last applied layout is remembered, so a holder that restarts comes back on the
/// layout the fleet is actually running rather than whatever its unit was built with.
fn layout_cache() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/rmng".to_string());
    std::path::Path::new(&home).join(".rmng/monitors")
}

/// Read the remembered layout, or `None` if there is none to trust.
///
/// The unit's `RMNG_MONITORS` is baked when the clone image is built, so on a clone whose
/// layout the operator has since changed it is simply wrong. The control-server corrects it
/// about a second after the daemon connects, but that second is a session build and a swap
/// nobody asked for. Remembering the last applied layout removes both.
fn cached_layout() -> Option<Vec<MonitorCfg>> {
    let raw = std::fs::read_to_string(layout_cache()).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(crate::parse_monitors(Some(raw.to_string())))
}

/// Remember `cfg` in the same `WxH+X+Y[*]` form `RMNG_MONITORS` uses, so one parser reads
/// both. Best-effort: a clone that cannot write its home still runs, it just boots from the
/// unit's layout next time.
fn remember_layout(cfg: &[MonitorCfg]) {
    let spec = cfg
        .iter()
        .map(|m| format!("{}x{}+{}+{}{}", m.w, m.h, m.x, m.y, if m.primary { "*" } else { "" }))
        .collect::<Vec<_>>()
        .join(",");
    let path = layout_cache();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, format!("{spec}\n")) {
        tracing::warn!("could not remember the layout in {}: {e}", path.display());
    }
}

/// Run the holder until the process is killed.
pub async fn run(boot: Vec<MonitorCfg>, cursor_mode: u32) -> Result<()> {
    // Bind before building the session: a second holder must fail here rather than after it
    // has created a duplicate set of monitors on the desktop.
    let path = wire::holder::socket_path();
    let listener = ipc::Listener::bind(&path).with_context(|| format!("binding {path}"))?;
    tracing::info!("session holder listening on {path}");

    let mut cfg = match cached_layout() {
        Some(remembered) => {
            tracing::info!("booting on the remembered layout ({} monitor(s))", remembered.len());
            remembered
        }
        None => boot,
    };
    let mut generation: u64 = 1;
    let mut session = build_session(&cfg, cursor_mode).await?;
    // A fast restart can race a PREVIOUS holder's teardown, whose monitors die
    // asynchronously after its connection dropped.
    wait_monitors_settle(cfg.len()).await;
    apply_layout(&cfg).await;
    remember_layout(&cfg);

    let active: ActiveSession = Arc::new(tokio::sync::Mutex::new(SessionRuntime {
        rd: session.rd.clone(),
        conn: session.conn.clone(),
        streams: stream_map(&session),
    }));

    // Outbound: one queue, drained by a thread that writes to the current daemon connection.
    // A thread rather than a task because `Conn::send` blocks.
    let current: Arc<std::sync::Mutex<Option<Arc<ipc::Conn>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let (out, mut out_rx) = unbounded_channel::<FromHolder>();
    {
        let current = current.clone();
        std::thread::Builder::new().name("holder-out".into()).spawn(move || {
            while let Some(msg) = out_rx.blocking_recv() {
                let conn = current.lock().unwrap().clone();
                match conn {
                    Some(c) => {
                        if let Err(e) = c.send(&msg) {
                            tracing::warn!("sending to the daemon failed: {e:#}");
                        }
                    }
                    None => tracing::debug!("no daemon connected; dropping a holder message"),
                }
            }
        })?;
    }

    // Input injection, off a queue so a slow D-Bus call never stalls the control loop.
    let (input_tx, input_rx) = unbounded_channel::<InputMsg>();
    tokio::spawn(inject_loop(active.clone(), input_rx));

    // Clipboard bridge: reads the current session per operation, answers on `out`.
    let (clip_tx, clip_rx) = unbounded_channel::<clipboard::FromServer>();
    tokio::spawn(clipboard::run(active.clone(), out.clone(), clip_rx));

    // Accept loop. One daemon at a time: a new connection shuts the previous one down, which
    // ends its reader thread.
    let (conn_tx, mut conn_rx) = unbounded_channel::<Arc<ipc::Conn>>();
    std::thread::Builder::new().name("holder-accept".into()).spawn(move || {
        loop {
            match listener.accept() {
                Ok(c) => {
                    if conn_tx.send(Arc::new(c)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!("accept failed: {e:#}");
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    })?;

    let (msg_tx, mut msg_rx) = unbounded_channel::<ToHolder>();
    let (ready_tx, mut ready_rx) = unbounded_channel::<u64>();
    // gnome-shell restarting closes the session under us. The watcher carries the generation
    // it belongs to, because our own `stop` during a swap raises `Closed` as well.
    let (closed_tx, mut closed_rx) = unbounded_channel::<u64>();
    watch_closed(&session, generation, closed_tx.clone());

    loop {
        tokio::select! {
            Some(conn) = conn_rx.recv() => {
                if let Some(old) = current.lock().unwrap().replace(conn.clone()) {
                    old.shutdown();
                }
                tracing::info!("daemon connected");
                spawn_reader(conn, msg_tx.clone(), ready_tx.clone());
            }
            Some(msg) = msg_rx.recv() => {
                match msg {
                    ToHolder::Hello { proto } => {
                        if proto != PROTO_VERSION {
                            tracing::warn!(
                                "daemon speaks holder protocol {proto}, this holder speaks \
                                 {PROTO_VERSION}; it should restart us"
                            );
                        }
                        let _ = out.send(FromHolder::HelloOk {
                            proto: PROTO_VERSION,
                            generation,
                            monitors: describe(&session, &cfg),
                        });
                    }
                    ToHolder::Input(m) => {
                        let _ = input_tx.send(m);
                    }
                    ToHolder::SetLayout { monitors } => {
                        let desired = monitors_from_specs(&monitors);
                        if desired == cfg {
                            tracing::debug!("layout unchanged; keeping the monitors we hold");
                            continue;
                        }
                        match swap(
                            &mut session, &active, &mut generation, &desired, cursor_mode,
                            &out, &mut ready_rx, &closed_tx,
                        ).await {
                            Ok(()) => {
                                remember_layout(&desired);
                                cfg = desired;
                            }
                            Err(e) => tracing::warn!("layout swap failed: {e:#}"),
                        }
                    }
                    ToHolder::ClipboardOffer(o) => {
                        let _ = clip_tx.send(clipboard::FromServer::Offer(o));
                    }
                    ToHolder::ClipboardRequest(r) => {
                        let _ = clip_tx.send(clipboard::FromServer::Request(r));
                    }
                    ToHolder::ClipboardData(d) => {
                        let _ = clip_tx.send(clipboard::FromServer::Data(d));
                    }
                    // `CaptureReady` is routed to `ready_rx` by the reader.
                    ToHolder::CaptureReady { .. } | ToHolder::Unknown => {}
                }
            }
            Some(closed_gen) = closed_rx.recv() => {
                if closed_gen != generation {
                    continue; // a session we stopped ourselves during a swap
                }
                tracing::warn!("Mutter closed the session (gnome-shell restart?); rebuilding");
                if let Err(e) = swap(
                    &mut session, &active, &mut generation, &cfg.clone(), cursor_mode,
                    &out, &mut ready_rx, &closed_tx,
                ).await {
                    tracing::error!("rebuilding the session failed: {e:#}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
}

/// Build a session for `cfg` and log what came up.
async fn build_session(cfg: &[MonitorCfg], cursor_mode: u32) -> Result<Session> {
    let sizes: Vec<(u32, u32)> = cfg.iter().map(|m| (m.w, m.h)).collect();
    let session = mutter::setup_with_cursor_mode(&sizes, cursor_mode).await?;
    if let Err(e) = session.rd.enable_clipboard(HashMap::new()).await {
        tracing::warn!("EnableClipboard failed (clipboard sync off): {e}");
    }
    tracing::info!("session ready: {} virtual monitor(s)", session.monitors.len());
    if std::env::var_os("RMNG_NUDGE").is_some() {
        nudge_cursor(&session);
    }
    Ok(session)
}

/// Pair each of the session's monitors with its configured placement.
///
/// `RecordVirtual` was called in slot order, so `monitor_id` indexes `cfg`. A session that
/// somehow came up with fewer monitors than configured pairs what it has.
fn describe(session: &Session, cfg: &[MonitorCfg]) -> Vec<HolderMonitor> {
    session
        .monitors
        .iter()
        .filter_map(|m| {
            let c = cfg.get(m.monitor_id as usize)?;
            Some(HolderMonitor {
                monitor_id: m.monitor_id,
                node_id: m.node_id,
                width: m.width,
                height: m.height,
                x: c.x,
                y: c.y,
                primary: c.primary,
            })
        })
        .collect()
}

fn stream_map(session: &Session) -> HashMap<u32, String> {
    session.monitors.iter().map(|m| (m.monitor_id, m.stream_path.clone())).collect()
}

/// Forward every message from one daemon connection, routing `CaptureReady` to its own
/// channel so a swap can wait for it while the control loop stays blocked inside the swap.
fn spawn_reader(conn: Arc<ipc::Conn>, msgs: UnboundedSender<ToHolder>, ready: UnboundedSender<u64>) {
    std::thread::Builder::new()
        .name("holder-in".into())
        .spawn(move || {
            loop {
                match conn.recv::<ToHolder>() {
                    Ok(ToHolder::CaptureReady { generation }) => {
                        if ready.send(generation).is_err() {
                            return;
                        }
                    }
                    Ok(m) => {
                        if msgs.send(m).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::info!("daemon disconnected: {e}");
                        return;
                    }
                }
            }
        })
        .expect("spawning the holder reader thread");
}

/// Tell the control loop when Mutter closes `session`, tagged with the generation it holds.
fn watch_closed(session: &Session, generation: u64, tx: UnboundedSender<u64>) {
    let rd = session.rd.clone();
    tokio::spawn(async move {
        let Ok(mut sig) = rd.receive_closed().await else { return };
        if sig.next().await.is_some() {
            let _ = tx.send(generation);
        }
    });
}

/// Inject input events one at a time against whichever session is current.
async fn inject_loop(
    active: ActiveSession,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<InputMsg>,
) {
    while let Some(msg) = rx.recv().await {
        // Snapshot the current session's `rd` (and this event's stream, for PointerMove)
        // under a SHORT lock, then drop the guard BEFORE the D-Bus await, so a slow notify
        // never holds the lock a swap needs.
        let (rd, stream) = {
            let rt = active.lock().await;
            let stream = match &msg {
                InputMsg::PointerMove { monitor_id, .. } => rt.streams.get(monitor_id).cloned(),
                _ => None,
            };
            (rt.rd.clone(), stream)
        };
        let r = match msg {
            InputMsg::PointerMove { x, y, .. } => match stream {
                Some(s) => rd.notify_pointer_motion_absolute(&s, x, y).await,
                None => Ok(()),
            },
            InputMsg::PointerRelative { dx, dy } => rd.notify_pointer_motion_relative(dx, dy).await,
            InputMsg::Button { button, pressed } => rd.notify_pointer_button(button, pressed).await,
            InputMsg::Axis { axis, step } => rd.notify_pointer_axis_discrete(axis, step).await,
            InputMsg::AxisContinuous { dx, dy, flags } => {
                rd.notify_pointer_axis(dx, dy, flags).await
            }
            InputMsg::Key { keysym, pressed } => rd.notify_keyboard_keysym(keysym, pressed).await,
            InputMsg::KeyCode { keycode, pressed } => {
                rd.notify_keyboard_keycode(keycode, pressed).await
            }
        };
        if let Err(e) = r {
            tracing::warn!("input inject failed: {e}");
        }
    }
}

/// Make-before-break session swap: build a fresh session with the full `desired` monitor
/// set, let the daemon start capture on it, re-point input and clipboard at it, THEN stop
/// the old session. Because the new outputs exist alongside the old until that stop,
/// gnome-shell never sees zero monitors and windows keep their positions.
#[allow(clippy::too_many_arguments)]
async fn swap(
    session: &mut Session,
    active: &ActiveSession,
    generation: &mut u64,
    desired: &[MonitorCfg],
    cursor_mode: u32,
    out: &Out,
    ready: &mut tokio::sync::mpsc::UnboundedReceiver<u64>,
    closed: &UnboundedSender<u64>,
) -> Result<()> {
    // 1. Build the NEW full session — its outputs appear ALONGSIDE the old.
    //    monitor_id == slot index.
    let new = build_session(desired, cursor_mode).await?;

    // 2. Point input, clipboard and the MCP at the new session BEFORE dropping the old.
    //    Setting `conn` here makes this handle the sole long-lived owner of the old conn.
    {
        let mut rt = active.lock().await;
        rt.rd = new.rd.clone();
        rt.conn = new.conn.clone();
        rt.streams = stream_map(&new);
    }

    // 3. Hand the daemon the new nodes and wait for capture to be running on them. Stale
    //    acknowledgements from an earlier generation are drained, not counted.
    *generation += 1;
    let new_gen = *generation;
    watch_closed(&new, new_gen, closed.clone());
    let _ = out.send(FromHolder::Monitors { generation: new_gen, monitors: describe(&new, desired) });
    match tokio::time::timeout(CAPTURE_READY_TIMEOUT, async {
        while let Some(g) = ready.recv().await {
            if g == new_gen {
                return;
            }
        }
    })
    .await
    {
        Ok(()) => tracing::debug!("daemon is capturing generation {new_gen}"),
        Err(_) => tracing::warn!(
            "no CaptureReady for generation {new_gen} after {}s; finishing the swap anyway",
            CAPTURE_READY_TIMEOUT.as_secs()
        ),
    }

    // 4. Stop the OLD session, WAIT for its monitors to actually disappear (stop is
    //    asynchronous — see `wait_monitors_settle`: applying against lingering dying
    //    connectors crashed gnome-shell live), then position the new monitors. Only after
    //    settle do just the new connectors exist, making the match unambiguous.
    let _ = session.stop().await;
    // Explicitly CLOSE the old session's bus connection. Refcount-drop can never close it:
    // the clipboard signal tasks are parked in `sig.next()` holding proxy clones of this very
    // connection, so without close() their streams never end, they never re-subscribe against
    // the NEW session in `active`, and both signal-driven flows stay wired to the dead
    // session's object path forever. close() ends those streams and reclaims the old fd.
    let _ = session.conn.clone().close().await;
    wait_monitors_settle(desired.len()).await;
    apply_layout(desired).await;

    // 5. The old monitors are gone: the daemon may drop their captures.
    let _ = out.send(FromHolder::SwapDone { generation: new_gen });
    *session = new;
    Ok(())
}

/// Oscillate the pointer so the damage-driven capture emits frames (`RMNG_NUDGE=1`, for
/// testing without a viewer). Never on in production: it would fight the operator's pointer.
pub(crate) fn nudge_cursor(session: &Session) {
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

// --- monitor layout (Mutter DisplayConfig) ---------------------------------

/// Wait until exactly `expected` monitors remain in `GetCurrentState` (the new session's).
///
/// Stopping a Mutter session tears its virtual monitors down ASYNCHRONOUSLY —
/// `session.stop()` returning does NOT mean the old connectors are gone. Calling
/// `apply_layout` while they linger matches desired slots against DYING connectors
/// (guaranteed when old and new sizes coincide, e.g. two same-size presets): Mutter then
/// either rejects the config as "stale information" (layout silently not applied) or, if the
/// teardown lands mid-apply, crashes gnome-shell outright — both found live on GNOME 48
/// (fleet-wide shell crash on a position-swapped same-size preset switch). The same race hits
/// the boot path when a restarted holder's predecessor session is still collapsing. On
/// timeout, warn and proceed: apply_layout stays best-effort.
async fn wait_monitors_settle(expected: usize) {
    for _ in 0..40 {
        if let Some((_, stdout)) = get_current_state().await {
            if parse_connectors(&stdout).len() == expected {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tracing::warn!("stale monitors still present after ~2s; applying layout anyway");
}

/// Apply the configured monitor layout (positions + primary) to Mutter via
/// `DisplayConfig.ApplyMonitorsConfig`. Uses `gdbus` (the nested ApplyMonitorsConfig GVariant
/// types are painful via zbus). Best-effort: logs + continues on failure (the monitors still
/// capture, just in Mutter's default left-to-right order). Connector names are read back from
/// `GetCurrentState` rather than assumed `Meta-<i>`, since after a session swap Mutter may
/// hand out connectors in a different order (or reuse different ones) than creation order.
async fn apply_layout(monitors: &[MonitorCfg]) {
    let Some((serial, stdout)) = get_current_state().await else {
        tracing::warn!("layout: couldn't read DisplayConfig state; leaving Mutter's default layout");
        return;
    };
    let mut available = parse_connectors(&stdout);
    let mut lm = String::from("[");
    let mut first = true;
    for m in monitors {
        // Pick (and consume) the first available connector whose current mode matches this
        // monitor's (w, h). `parse_connectors` returns creation order (ascending Meta-N), and
        // slots were RecordVirtual'd in order, so duplicate sizes map 1:1: slot i's connector
        // IS the one whose stream ships as monitor_id i.
        let Some(idx) = available.iter().position(|(_, w, h)| *w == m.w && *h == m.h) else {
            tracing::warn!(
                "layout: no Mutter connector currently at {}x{}; skipping that monitor",
                m.w,
                m.h
            );
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
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.DisplayConfig",
            "--object-path",
            "/org/gnome/Mutter/DisplayConfig",
            "--method",
            "org.gnome.Mutter.DisplayConfig.ApplyMonitorsConfig",
            &serial.to_string(),
            "1",
            &lm,
            "@a{sv} {}",
        ])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            tracing::info!("applied monitor layout ({} monitor(s))", monitors.len())
        }
        Ok(o) => tracing::warn!(
            "ApplyMonitorsConfig failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => tracing::warn!("ApplyMonitorsConfig spawn failed (gdbus missing?): {e}"),
    }
}

/// Call `DisplayConfig.GetCurrentState` and return `(serial, stdout)`. Used by `apply_layout`
/// to read back connector names from the current Mutter state.
async fn get_current_state() -> Option<(u32, String)> {
    let out = tokio::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.DisplayConfig",
            "--object-path",
            "/org/gnome/Mutter/DisplayConfig",
            "--method",
            "org.gnome.Mutter.DisplayConfig.GetCurrentState",
        ])
        .output()
        .await
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let idx = s.find("uint32 ")?;
    let serial = s[idx + 7..]
        .split(|c: char| !c.is_ascii_digit())
        .find(|t| !t.is_empty())?
        .parse()
        .ok()?;
    Some((serial, s))
}

/// Parse the text form of `DisplayConfig.GetCurrentState`'s stdout (as printed by `gdbus
/// call`) into `(connector, width, height)` for each monitor's *current* mode.
///
/// Shape (scales list length varies, `...` elided):
/// ```text
/// (uint32 3, [(('Meta-0', 'MetaVendor', 'Virtual remote monitor', '0x000001'),
///   [('2560x1440@60.000', 2560, 1440, 60.0, 1.0, [1.0, 1.25],
///     {'is-current': <true>, 'is-preferred': <true>})],
///   {'is-builtin': <false>, ...}), (('Meta-1', ...), [...], {...})], [...], {...})
/// ```
/// Each monitor group starts at a `('Meta-` (or other connector-prefixed) tuple; we split the
/// monitor-list on that boundary. Within a group, the connector is the first single-quoted
/// string; the current mode is whichever mode tuple's props contain `'is-current': <true>`,
/// and its `WxH` are the two integers immediately following that mode's `'WxH@rate'` id
/// string.
fn parse_connectors(get_current_state_stdout: &str) -> Vec<(String, u32, u32)> {
    let mut out = Vec::new();
    // Split into per-monitor chunks on `(('` (the start of each monitor's connector tuple),
    // which only occurs at monitor-group boundaries in this shape.
    for chunk in get_current_state_stdout.split("((").skip(1) {
        // The connector name is the first single-quoted string in the chunk.
        let Some(connector) = first_quoted(chunk) else {
            continue;
        };
        // Find the mode tuple containing `'is-current': <true>`; walk each `'<mode-id>', <w>,
        // <h>` occurrence and keep the one whose nearby props mention is-current.
        let Some(current_mode_end) = chunk.find("'is-current': <true>") else {
            continue;
        };
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
            let Some(end_quote) = after_quote.find('\'') else {
                break;
            };
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
    // Creation order, not enumeration order: apply_layout consumes the first size match per
    // slot, so duplicate sizes only map 1:1 to slots if this list ascends in creation order.
    // Mutter allocates virtual connector names `Meta-N` lowest-free, so sequential
    // RecordVirtual calls always get ascending N (verified live on GNOME 48) — while
    // GetCurrentState's enumeration order carries no such contract. Stable sort keeps
    // suffix-less names (never seen for virtual monitors) in enumeration order at the end.
    out.sort_by_key(|(c, _, _)| connector_index(c));
    out
}

/// The numeric suffix of a connector name (`"Meta-10"` → 10), recovering creation order for
/// [`parse_connectors`]. Names without one sort last.
fn connector_index(name: &str) -> u64 {
    name.rsplit_once('-').and_then(|(_, n)| n.parse().ok()).unwrap_or(u64::MAX)
}

/// The first single-quoted string in `s`, e.g. `"'Meta-0', 'MetaVendor'"` → `"Meta-0"`.
fn first_quoted(s: &str) -> Option<String> {
    let start = s.find('\'')? + 1;
    let end = s[start..].find('\'')? + start;
    Some(s[start..end].to_string())
}

/// Parse a mode id like `2560x1440@60.000` into `(2560, 1440)`; `None` if `s` isn't of that
/// shape (e.g. a connector/vendor/product/serial string encountered along the way).
fn parse_mode_id(s: &str) -> Option<(u32, u32)> {
    let (wh, _rate) = s.split_once('@')?;
    let (w, h) = wh.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
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
{'is-builtin': <false>})], [(0, 0, 1.0, 0, true, [('Meta-0', 'x', 'y', '0x1')], {}), \
(0, 0, 1.0, 0, false, [('Meta-1', 'x', 'y', '0x2')], {})], {'layout-mode': <uint32 1>})";
        let got = parse_connectors(blob);
        assert_eq!(
            got,
            vec![("Meta-0".to_string(), 2560, 1440), ("Meta-1".to_string(), 1920, 1080)]
        );
    }

    #[test]
    fn parse_connectors_orders_by_creation_not_enumeration() {
        // Two monitors at the SAME size: slot→connector matching in apply_layout consumes
        // the first match per slot, so this list must ascend in creation order (Meta-N)
        // whatever order GetCurrentState enumerated them in.
        let blob = "(uint32 5, [(('Meta-2', 'MetaVendor', 'Virtual remote monitor', '0x2'), \
[('1920x1080@60.000', 1920, 1080, 60.0, 1.0, [1.0], {'is-current': <true>})], \
{'is-builtin': <false>}), (('Meta-1', 'MetaVendor', 'Virtual remote monitor', '0x1'), \
[('1920x1080@60.000', 1920, 1080, 60.0, 1.0, [1.0], {'is-current': <true>})], \
{'is-builtin': <false>})], [(0, 0, 1.0, 0, true, [('Meta-1', 'x', 'y', '0x2')], {}), \
(1920, 0, 1.0, 0, false, [('Meta-2', 'x', 'y', '0x3')], {})], {'layout-mode': <uint32 1>})";
        let got: Vec<String> = parse_connectors(blob).into_iter().map(|(c, _, _)| c).collect();
        assert_eq!(got, vec!["Meta-1".to_string(), "Meta-2".to_string()]);
    }

    #[test]
    fn parse_connectors_picks_is_current_mode() {
        // Two modes offered, only the second is current.
        let blob = "(uint32 1, [(('Meta-0', 'MetaVendor', 'Virtual remote monitor', '0x000001'), \
[('3840x2160@60.000', 3840, 2160, 60.0, 1.0, [1.0], {'is-preferred': <true>}), \
('1920x1080@60.000', 1920, 1080, 60.0, 1.0, [1.0], {'is-current': <true>})], \
{'is-builtin': <false>})], [(0, 0, 1.0, uint32 0, true, [('Meta-0', 'x', 'y', '0x1')], @a{sv} {})], {'layout-mode': <uint32 1>})";
        assert_eq!(parse_connectors(blob), vec![("Meta-0".to_string(), 1920, 1080)]);
    }

    /// The whole point of the holder is that a daemon restart does not rebuild the session.
    /// The server pushes the active layout on every `Hello`, so the no-op push that follows
    /// each reconnect has to compare equal and change nothing.
    #[test]
    fn a_repushed_layout_is_the_same_config() {
        let specs = vec![
            wire::control::MonitorSpec { width: 1920, height: 1080, x: 0, y: 0, primary: true },
            wire::control::MonitorSpec { width: 1920, height: 1080, x: 1920, y: 0, primary: false },
        ];
        let once = monitors_from_specs(&specs);
        assert_eq!(once, monitors_from_specs(&specs));
        // A real change still differs.
        let mut moved = specs.clone();
        moved[1].x = 2560;
        assert_ne!(once, monitors_from_specs(&moved));
    }
}
