//! Headless-clone terminal plane.
//!
//! A headless clone (`Host.headless`) runs no desktop and streams no video. When such a clone
//! is the selected host and at least one viewer is connected, this plane proxies each of its
//! tmux sessions to the viewer over port 1: it enumerates `tmux list-sessions`, opens one
//! interactive `tmux attach` PTY per session via `docker exec` (bollard TTY exec), and pumps
//! bytes both ways. The viewer renders one terminal tab per session on its primary window.
//!
//! Wire (port 1): server→viewer [`wire::viewer::TermInit`] (tag 6, session list) and
//! [`wire::viewer::TermData`] (tag 7, output bytes); viewer→server [`wire::viewer::TermInput`],
//! [`wire::viewer::TermResize`], [`wire::viewer::TermNewSession`] (parsed in `mediaplane`'s
//! `read_viewer_input`, dispatched here).
//!
//! Lifecycle: exactly one clone is "active" at a time (the selected headless clone). Activation
//! spawns a manager task; deactivation drops it, which aborts the manager and every per-session
//! PTY pump (dropping the exec streams makes `tmux attach` see EOF and detach cleanly).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::App;
use crate::mediaplane::{Viewers, broadcast_json, T_TERM_DATA, T_TERM_INIT};

/// The clone user every session PTY runs as (uid 1000).
const CLONE_USER: &str = "rmng";
/// Initial PTY size before the viewer reports its real tab dimensions.
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 32;
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
}

impl TermPlane {
    pub fn new(app: App, viewers: Viewers, rt: tokio::runtime::Handle) -> Self {
        Self { app, viewers, rt, active: Mutex::new(None) }
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
                // current session list (a redundant TermInit for existing viewers is harmless).
                Some(a) if a.clone_id == clone_id => {
                    let sessions = a.sessions.lock().unwrap().clone();
                    broadcast_json(&self.viewers, T_TERM_INIT, &wire::viewer::TermInit { sessions });
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

    /// Resize every session's PTY (the viewer's tabs share one window size).
    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
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
        let manager = self.rt.spawn(run_manager(
            self.app.clone(),
            self.viewers.clone(),
            clone_id.clone(),
            cmd_rx,
            sessions.clone(),
        ));
        tracing::info!(target: "termplane", "terminal view activated for {clone_id}");
        Active { clone_id, cmd_tx, sessions, _manager: AbortOnDrop(manager) }
    }
}

/// One active clone's manager: enumerate sessions, keep a PTY pump per session, and dispatch
/// viewer commands. Ends (and cancels its session pumps) when its `Active` is dropped.
async fn run_manager(
    app: App,
    viewers: Viewers,
    clone_id: String,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    sessions: Arc<Mutex<Vec<String>>>,
) {
    let mut txs: HashMap<String, mpsc::UnboundedSender<SessionMsg>> = HashMap::new();
    let mut tasks: HashMap<String, AbortOnDrop> = HashMap::new();
    let mut size = (DEFAULT_COLS, DEFAULT_ROWS);

    reconcile(&app, &viewers, &clone_id, &mut txs, &mut tasks, &sessions, size).await;

    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = poll.tick() => {
                reconcile(&app, &viewers, &clone_id, &mut txs, &mut tasks, &sessions, size).await;
            }
            cmd = cmd_rx.recv() => match cmd {
                None => break,
                Some(Cmd::Input { session, data }) => {
                    if let Some(tx) = txs.get(&session) {
                        let _ = tx.send(SessionMsg::Data(data));
                    }
                }
                Some(Cmd::Resize { cols, rows }) => {
                    size = (cols, rows);
                    for tx in txs.values() {
                        let _ = tx.send(SessionMsg::Resize { cols, rows });
                    }
                }
                Some(Cmd::NewSession) => {
                    let name = next_session_name(&txs);
                    if let Err(e) = new_tmux_session(&app, &clone_id, &name).await {
                        tracing::warn!(target: "termplane", "new tmux session in {clone_id} failed: {e:#}");
                    }
                    reconcile(&app, &viewers, &clone_id, &mut txs, &mut tasks, &sessions, size).await;
                }
            }
        }
    }
}

/// Bring the running PTY-pump set in line with the clone's live tmux sessions: spawn pumps for
/// new sessions, drop (abort) pumps for vanished ones, and broadcast a fresh `TermInit` when the
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
        broadcast_json(viewers, T_TERM_INIT, &wire::viewer::TermInit { sessions: current });
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
    // `attach-session` (not `new-session`): the session already exists. `-t` selects it.
    let cmd = [
        "tmux".to_string(),
        "attach-session".to_string(),
        "-t".to_string(),
        session.clone(),
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
