//! Headless-clone terminal plane.
//!
//! A headless clone (`Host.headless`) runs no desktop and streams no video. When such a clone
//! is the selected host and at least one viewer is connected, this plane proxies each of its
//! tmux sessions to the viewer over port 1: it enumerates `tmux list-sessions`, opens one
//! interactive `tmux attach` PTY per session via `docker exec` (bollard TTY exec), and pumps
//! bytes both ways. The viewer renders one terminal tab per session on its primary window.
//!
//! Wire (port 1): the session list rides in the tag-3 [`wire::viewer::ViewSpec`]
//! (`ViewContent::Terminal`); per-session output is [`wire::viewer::TermData`] (tag 7). Viewer→
//! server [`wire::viewer::TermInput`], [`wire::viewer::TermResize`],
//! [`wire::viewer::TermNewSession`] (parsed in `mediaplane`'s `read_viewer_input`, dispatched here).
//!
//! Lifecycle: exactly one clone is "active" at a time (the selected headless clone). Activation
//! spawns a manager task; deactivation drops it, which aborts the manager and every per-session
//! PTY pump (dropping the exec streams makes `tmux attach` see EOF and detach cleanly).
//!
//! Sizing: PTY attach is **deferred** until the viewer reports its real tab dimensions (a
//! `TermResize`), so every session's tmux is born at the true grid instead of a default that
//! visibly corrects a moment later. The session list (the `ViewSpec`) is announced immediately so
//! the viewer can build + measure its terminal.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::App;
use crate::mediaplane::{Viewers, broadcast_json, configured_monitors, T_TERM_DATA, T_VIEWSPEC};

/// The clone user every session PTY runs as (uid 1000).
const CLONE_USER: &str = "rmng";
/// argv[0] our `tmux attach` clients run under, so we can reap *our* orphans without touching a
/// human's `tmux attach`. Docker exec processes are NOT killed when their stream is dropped (and a
/// dead-PTY client ignores `detach-client`), so each attach lingers as a stale tmux client; a pile
/// of them collapses `window-size latest` to a tiny size. We reap these at activation.
const PROXY_MARKER: &str = "rmng-tmux-proxy";
/// Fallback PTY size if the viewer never reports its real dimensions within [`ATTACH_WAIT`].
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 32;
/// How long to wait for the viewer's first `TermResize` before attaching PTYs at [`DEFAULT_COLS`]
/// × [`DEFAULT_ROWS`]. The viewer normally reports within one frame of receiving the session list,
/// so PTYs are almost always born at the true grid; this bound just prevents a stuck viewer from
/// leaving the terminal permanently blank.
const ATTACH_WAIT: Duration = Duration::from_millis(750);
/// How often the manager re-enumerates tmux sessions to pick up ones the agent/operator
/// created (or destroyed) inside the clone.
const POLL: Duration = Duration::from_millis(1500);

/// Abort a spawned task when this handle is dropped — how deactivation cancels the manager and
/// how the manager cancels a vanished session's pump.
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A viewer→server command routed to the active clone's manager task.
enum Cmd {
    Input { session: String, data: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    NewSession,
}

/// A manager→session-pump message.
enum SessionMsg {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// The currently-active headless clone's terminal state.
struct Active {
    clone_id: String,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    /// The last-broadcast session list, so a newly-connected viewer can be re-primed without
    /// waiting for the next poll/change.
    sessions: Arc<Mutex<Vec<String>>>,
    _manager: AbortOnDrop,
}

/// Shared handle, constructed once by the media plane and threaded into the viewer connect
/// path, the selection watcher, and each viewer's input reader.
pub struct TermPlane {
    app: App,
    viewers: Viewers,
    rt: tokio::runtime::Handle,
    active: Mutex<Option<Active>>,
    /// The viewer's last-reported terminal grid, remembered across (de)activations. The terminal
    /// window is a stable viewer window whose size carries over between headless clones and across
    /// reconnects, but the viewer only emits a `TermResize` when that widget is (re)allocated — not
    /// on a headless→headless switch. So on activation we attach PTYs immediately at this size
    /// instead of waiting for a resize that won't come; `None` (first-ever select) falls back to
    /// the deferred path.
    last_size: Mutex<Option<(u16, u16)>>,
}

impl TermPlane {
    pub fn new(app: App, viewers: Viewers, rt: tokio::runtime::Handle) -> Self {
        Self { app, viewers, rt, active: Mutex::new(None), last_size: Mutex::new(None) }
    }

    /// Re-evaluate whether the terminal view should be running: it is active iff the selected
    /// clone is headless AND ≥1 viewer is connected. Called on selection change, viewer
    /// connect, and viewer disconnect. Idempotent; safe to over-call.
    pub fn on_viewers_changed(&self) {
        let selected = self.selected_headless();
        let has_viewers = !self.viewers.lock().unwrap().is_empty();
        let mut active = self.active.lock().unwrap();
        match (selected, has_viewers) {
            (Some(clone_id), true) => match active.as_ref() {
                // Already running for this clone: re-prime any freshly-connected viewer with the
                // current session list (a redundant ViewSpec for existing viewers is harmless).
                Some(a) if a.clone_id == clone_id => {
                    let sessions = a.sessions.lock().unwrap().clone();
                    broadcast_view_spec(&self.app, &self.viewers, &clone_id, sessions);
                }
                _ => {
                    *active = Some(self.activate(clone_id));
                }
            },
            // Not a headless selection, or no viewers: tear down. Dropping `Active` aborts the
            // manager (and, transitively, every session pump).
            _ => {
                if active.take().is_some() {
                    tracing::info!(target: "termplane", "terminal view deactivated");
                }
            }
        }
    }

    /// Route a viewer keystroke/paste to a session of the active clone.
    pub fn input(&self, session: String, data: Vec<u8>) {
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            let _ = a.cmd_tx.send(Cmd::Input { session, data });
        }
    }

    /// Resize every session's PTY (the viewer's tabs share one window size). Also records the size
    /// so a later (re)activation can attach at it without waiting for a fresh report.
    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        *self.last_size.lock().unwrap() = Some((cols, rows));
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            let _ = a.cmd_tx.send(Cmd::Resize { cols, rows });
        }
    }

    /// The tab-bar "+": create a new tmux session in the active clone.
    pub fn new_session(&self) {
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            let _ = a.cmd_tx.send(Cmd::NewSession);
        }
    }

    /// The selected host's id iff it is a headless clone.
    fn selected_headless(&self) -> Option<String> {
        let sel = self.app.store.selected()?;
        self.app
            .store
            .get()
            .hosts
            .into_iter()
            .find(|h| h.id == sel && h.headless)
            .map(|h| h.id)
    }

    fn activate(&self, clone_id: String) -> Active {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let sessions = Arc::new(Mutex::new(Vec::new()));
        let init_size = *self.last_size.lock().unwrap();
        let manager = self.rt.spawn(run_manager(
            self.app.clone(),
            self.viewers.clone(),
            clone_id.clone(),
            cmd_rx,
            sessions.clone(),
            init_size,
        ));
        tracing::info!(target: "termplane", "terminal view activated for {clone_id}");
        Active { clone_id, cmd_tx, sessions, _manager: AbortOnDrop(manager) }
    }
}

/// One active clone's manager: enumerate sessions, keep a PTY pump per session, and dispatch
/// viewer commands. Ends (and cancels its session pumps) when its `Active` is dropped.
///
/// PTY attach is deferred until `size` is known — the viewer's first `TermResize`, or the
/// [`ATTACH_WAIT`] fallback — so every session's tmux is born at the true grid. Until then the
/// session list is still announced (via [`announce_sessions`]) so the viewer builds + measures
/// its terminal and reports that size.
async fn run_manager(
    app: App,
    viewers: Viewers,
    clone_id: String,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    sessions: Arc<Mutex<Vec<String>>>,
    init_size: Option<(u16, u16)>,
) {
    let mut txs: HashMap<String, mpsc::UnboundedSender<SessionMsg>> = HashMap::new();
    let mut tasks: HashMap<String, AbortOnDrop> = HashMap::new();
    // The size the PTYs are (to be) attached at, and whether we've attached yet. We do NOT attach
    // at activation even when `size` is already known (from a remembered `init_size`): attaching
    // immediately races the docker exec's TTY setup and can strand the PTY at the 80x24 default.
    // Instead we always wait for the viewer's first `TermResize` (it rebuilds + re-measures its
    // terminal on any clone change, so one arrives within ~a frame) or the `ATTACH_WAIT` fallback,
    // which is the timing that reliably wins the resize race. `init_size` only seeds the fallback.
    let mut size: Option<(u16, u16)> = init_size;
    let mut attached = false;

    // Reap orphaned attach clients from prior viewer sessions BEFORE attaching fresh, so
    // `window-size latest` is driven only by our current clients (else a pile of stale clients
    // clamps the window to a tiny size — the "tmux not resized to the window" bug).
    reap_orphan_clients(&app, &clone_id).await;

    // Announce the session list immediately (no pumps yet) so the viewer can build + measure.
    announce_sessions(&app, &viewers, &clone_id, &sessions).await;
    let attach_at = tokio::time::Instant::now() + ATTACH_WAIT;

    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // Fallback: the viewer never reported a size — attach at the remembered size (or the
            // default) so the terminal isn't left blank. Disabled once attached.
            _ = tokio::time::sleep_until(attach_at), if !attached => {
                let sz = size.unwrap_or((DEFAULT_COLS, DEFAULT_ROWS));
                size = Some(sz);
                attached = true;
                reconcile(&app, &viewers, &clone_id, &mut txs, &mut tasks, &sessions, sz).await;
            }
            _ = poll.tick() => {
                match (attached, size) {
                    // Pumps running: reconcile them against live tmux sessions.
                    (true, Some(sz)) => reconcile(&app, &viewers, &clone_id, &mut txs, &mut tasks, &sessions, sz).await,
                    // Still waiting to attach: keep the announced session list fresh (no pumps).
                    _ => announce_sessions(&app, &viewers, &clone_id, &sessions).await,
                }
            }
            cmd = cmd_rx.recv() => match cmd {
                None => break,
                Some(Cmd::Input { session, data }) => {
                    if let Some(tx) = txs.get(&session) {
                        let _ = tx.send(SessionMsg::Data(data));
                    }
                }
                Some(Cmd::Resize { cols, rows }) => {
                    size = Some((cols, rows));
                    if !attached {
                        // First real size: attach every PTY now, born at the true grid.
                        attached = true;
                        reconcile(&app, &viewers, &clone_id, &mut txs, &mut tasks, &sessions, (cols, rows)).await;
                    } else {
                        for tx in txs.values() {
                            let _ = tx.send(SessionMsg::Resize { cols, rows });
                        }
                    }
                }
                Some(Cmd::NewSession) => {
                    let name = next_session_name(&txs);
                    if let Err(e) = new_tmux_session(&app, &clone_id, &name).await {
                        tracing::warn!(target: "termplane", "new tmux session in {clone_id} failed: {e:#}");
                    }
                    match (attached, size) {
                        (true, Some(sz)) => reconcile(&app, &viewers, &clone_id, &mut txs, &mut tasks, &sessions, sz).await,
                        _ => announce_sessions(&app, &viewers, &clone_id, &sessions).await,
                    }
                }
            }
        }
    }
}

/// Broadcast the current tmux session list to all viewers as a `Terminal` [`wire::viewer::ViewSpec`]
/// (tag 3): the stable window set (configured monitors) + the owning clone id + the session names
/// for the tab bar. The clone id lets the viewer rebuild the terminal when the selection moves to a
/// different headless clone (session names alone can't distinguish two clones' `main` sessions).
fn broadcast_view_spec(app: &App, viewers: &Viewers, clone_id: &str, sessions: Vec<String>) {
    let spec = wire::viewer::ViewSpec {
        monitors: configured_monitors(&app.config()),
        content: wire::viewer::ViewContent::Terminal { clone: clone_id.to_string(), sessions },
    };
    broadcast_json(viewers, T_VIEWSPEC, &spec);
}

/// Enumerate the clone's tmux sessions (creating a default `main` if none exist) and broadcast the
/// `Terminal` `ViewSpec` when the set changed — **without** attaching any PTYs. Used before the
/// viewer reports its size so it can build + measure its terminal; pumps are spawned later, at the
/// true grid, by [`reconcile`].
async fn announce_sessions(
    app: &App,
    viewers: &Viewers,
    clone_id: &str,
    sessions: &Arc<Mutex<Vec<String>>>,
) {
    let mut current = list_sessions(app, clone_id).await;
    if current.is_empty() {
        if let Err(e) = new_tmux_session(app, clone_id, "main").await {
            tracing::warn!(target: "termplane", "default tmux session in {clone_id} failed: {e:#}");
        }
        current = list_sessions(app, clone_id).await;
    }
    let changed = {
        let mut g = sessions.lock().unwrap();
        if *g != current {
            *g = current.clone();
            true
        } else {
            false
        }
    };
    if changed {
        broadcast_view_spec(app, viewers, clone_id, current);
    }
}

/// Bring the running PTY-pump set in line with the clone's live tmux sessions: spawn pumps for
/// new sessions, drop (abort) pumps for vanished ones, and broadcast a fresh `ViewSpec` when the
/// list changed. Ensures a default `main` session exists so there is always at least one tab.
async fn reconcile(
    app: &App,
    viewers: &Viewers,
    clone_id: &str,
    txs: &mut HashMap<String, mpsc::UnboundedSender<SessionMsg>>,
    tasks: &mut HashMap<String, AbortOnDrop>,
    sessions: &Arc<Mutex<Vec<String>>>,
    size: (u16, u16),
) {
    let mut current = list_sessions(app, clone_id).await;
    if current.is_empty() {
        if let Err(e) = new_tmux_session(app, clone_id, "main").await {
            tracing::warn!(target: "termplane", "default tmux session in {clone_id} failed: {e:#}");
        }
        current = list_sessions(app, clone_id).await;
    }

    for s in &current {
        if !txs.contains_key(s) {
            let (tx, rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(pump_session(
                app.clone(),
                viewers.clone(),
                clone_id.to_string(),
                s.clone(),
                rx,
                size,
            ));
            txs.insert(s.clone(), tx);
            tasks.insert(s.clone(), AbortOnDrop(task));
        }
    }
    // Drop pumps whose session no longer exists (AbortOnDrop aborts them on removal).
    txs.retain(|k, _| current.contains(k));
    tasks.retain(|k, _| current.contains(k));

    let changed = {
        let mut g = sessions.lock().unwrap();
        if *g != current {
            *g = current.clone();
            true
        } else {
            false
        }
    };
    if changed {
        broadcast_view_spec(app, viewers, clone_id, current);
    }
}

/// Proxy one tmux session: `tmux attach` in a TTY exec; forward its output to viewers as
/// `TermData` and viewer input/resize into the PTY. Returns when the session ends (attach EOF)
/// or the manager drops our command channel.
async fn pump_session(
    app: App,
    viewers: Viewers,
    clone_id: String,
    session: String,
    mut rx: mpsc::UnboundedReceiver<SessionMsg>,
    size: (u16, u16),
) {
    // `attach-session` (not `new-session`): the session already exists. `-t` selects it. Run it
    // under a marker argv[0] (via bash `exec -a`) so `reap_orphan_clients` can later kill our own
    // orphaned attaches without disturbing a human's `tmux attach`.
    let cmd = [
        "bash".to_string(),
        "-c".to_string(),
        format!("exec -a {PROXY_MARKER} tmux attach-session -t {session}"),
    ];
    let tty = match app.docker.exec_tty(&clone_id, &cmd, CLONE_USER, size.0, size.1).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(target: "termplane", "attach {clone_id}/{session} failed: {e:#}");
            return;
        }
    };
    let crate::docker::TtyExec { id, mut output, mut input } = tty;
    // Apply the manager's current size once more in case it changed between spawn and attach.
    let _ = app.docker.resize_exec(&id, size.0, size.1).await;

    loop {
        tokio::select! {
            chunk = output.next() => match chunk {
                Some(Ok(log)) => {
                    let bytes = log.into_bytes();
                    if !bytes.is_empty() {
                        broadcast_json(
                            &viewers,
                            T_TERM_DATA,
                            &wire::viewer::TermData { session: session.clone(), data: bytes.to_vec() },
                        );
                    }
                }
                // EOF or a stream error: the tmux client detached or the session was killed.
                _ => break,
            },
            msg = rx.recv() => match msg {
                Some(SessionMsg::Data(d)) => {
                    if input.write_all(&d).await.is_err() {
                        break;
                    }
                    let _ = input.flush().await;
                }
                Some(SessionMsg::Resize { cols, rows }) => {
                    let _ = app.docker.resize_exec(&id, cols, rows).await;
                }
                None => break,
            }
        }
    }
    tracing::debug!(target: "termplane", "session pump ended {clone_id}/{session}");
}

/// Kill our own orphaned `tmux attach` clients (argv[0] == [`PROXY_MARKER`]) left by prior viewer
/// sessions. `docker exec` processes aren't terminated when their stream is dropped, and a
/// dead-PTY client ignores `detach-client`, so `kill` is the only reliable reaper. Matched by the
/// marker so a human's plain `tmux attach` is left alone. Best-effort; `pkill` exits non-zero when
/// nothing matches, which is fine.
async fn reap_orphan_clients(app: &App, clone_id: &str) {
    let cmd = ["pkill".to_string(), "-9".to_string(), "-f".to_string(), PROXY_MARKER.to_string()];
    let _ = app.docker.exec_capture(clone_id, &cmd, CLONE_USER, None, &[], None).await;
}

/// `tmux list-sessions -F '#{session_name}'` → the session names (empty on no server / error).
async fn list_sessions(app: &App, clone_id: &str) -> Vec<String> {
    let cmd = [
        "tmux".to_string(),
        "list-sessions".to_string(),
        "-F".to_string(),
        "#{session_name}".to_string(),
    ];
    match app.docker.exec_capture(clone_id, &cmd, CLONE_USER, None, &[], None).await {
        Ok(r) => r
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        Err(e) => {
            tracing::debug!(target: "termplane", "list-sessions {clone_id}: {e:#}");
            Vec::new()
        }
    }
}

/// `tmux new-session -d -s <name> -c <home>` (detached, starting in the clone user's home).
async fn new_tmux_session(app: &App, clone_id: &str, name: &str) -> anyhow::Result<()> {
    let cmd = [
        "tmux".to_string(),
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        name.to_string(),
        "-c".to_string(),
        format!("/home/{CLONE_USER}"),
    ];
    let r = app.docker.exec_capture(clone_id, &cmd, CLONE_USER, None, &[], None).await?;
    if r.exit_code != 0 {
        anyhow::bail!("tmux new-session exit {}: {}", r.exit_code, r.stderr.trim());
    }
    Ok(())
}

/// The next unused `work-N` name for a "+"-created session.
fn next_session_name(txs: &HashMap<String, mpsc::UnboundedSender<SessionMsg>>) -> String {
    (1..)
        .map(|i| format!("work-{i}"))
        .find(|name| !txs.contains_key(name))
        .expect("infinite range always yields a free name")
}
