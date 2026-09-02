//! The daemon's handle on the session the holder owns.
//!
//! Everything Mutter will only answer for the connection that created the session (input
//! injection, the clipboard, the monitors themselves) goes through here as a message. The
//! one D-Bus connection the daemon keeps for itself is the session bus, for
//! `org.gnome.Shell.Eval`: the window tools are a plain gnome-shell method with no
//! per-connection check, so they need no holder round trip.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use wire::holder::{FromHolder, PROTO_VERSION, ToHolder};
use wire::socket::InputMsg;

use crate::ipc;

/// How long to wait for the holder's socket to appear before giving up on this attempt.
///
/// The holder builds its Mutter session before it binds, so a cold boot has the daemon
/// waiting through session setup. Ten tries at 500 ms covers that with room to spare.
const CONNECT_TRIES: u32 = 10;
const CONNECT_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the holder gets to answer `Hello` before we treat it as wedged.
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) struct Holder {
    conn: Arc<ipc::Conn>,
    /// Messages from the holder, minus the `HelloOk` consumed by the handshake, which is
    /// re-queued so the control loop sees the monitor set the same way it sees every later
    /// generation.
    inbox: std::sync::Mutex<Option<UnboundedReceiver<FromHolder>>>,
    /// Whether this daemon started the holder rather than finding one already up. See
    /// [`wire::socket::Hello::fresh_session`].
    fresh: bool,
    /// One entry per outstanding [`Holder::sync_input`], completed by the reader when its
    /// acknowledgement arrives.
    waiters: Waiters,
    next_sync: std::sync::atomic::AtomicU64,
}

/// Barrier acknowledgements the reader has yet to deliver, keyed by the id that asked.
type Waiters = Arc<std::sync::Mutex<HashMap<u64, tokio::sync::oneshot::Sender<()>>>>;

/// How long [`Holder::sync_input`] waits before giving up on an acknowledgement.
///
/// The holder answers from its injection queue, so this only expires when that queue is
/// wedged, and a click that presses a frame early beats one that never presses at all.
const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

impl Holder {
    /// Connect to the holder, starting it if it is not running and restarting it if it
    /// speaks a different protocol version.
    ///
    /// A version mismatch costs one window reset on that release, which is why the version
    /// is bumped only for a real protocol change. Every other release reconnects to the
    /// holder that was already running, and the clone's windows never move.
    pub async fn connect() -> Result<Self> {
        let path = wire::holder::socket_path();
        let mut fresh = false;
        let conn = match dial(&path).await {
            Some(c) => c,
            None => {
                tracing::warn!("no session holder at {path}; starting the unit");
                fresh = true;
                start_unit("start").await;
                dial(&path)
                    .await
                    .with_context(|| format!("no session holder at {path} after starting it"))?
            }
        };

        let holder = Self::handshake(conn).await?.map(|h| h.with_fresh(fresh));
        let Some(holder) = holder else {
            tracing::warn!(
                "the session holder speaks a different protocol version; restarting it. \
                 This release resets window positions once."
            );
            start_unit("restart").await;
            let conn = dial(&path)
                .await
                .with_context(|| format!("no session holder at {path} after restarting it"))?;
            // A restarted holder is as new as one this daemon started: its session is seconds
            // old and the windows it inherited have already moved.
            return Ok(Self::handshake(conn)
                .await?
                .context("the restarted session holder still speaks a different protocol")?
                .with_fresh(true));
        };
        Ok(holder)
    }

    fn with_fresh(mut self, fresh: bool) -> Self {
        self.fresh = fresh;
        self
    }

    /// Whether this daemon brought the holder up, rather than joining one that was already
    /// holding a desktop. See [`wire::socket::Hello::fresh_session`].
    pub fn fresh_session(&self) -> bool {
        self.fresh
    }

    /// Say hello and read the answer. `Ok(None)` means the versions differ.
    async fn handshake(conn: ipc::Conn) -> Result<Option<Self>> {
        let conn = Arc::new(conn);
        conn.send(&ToHolder::Hello { proto: PROTO_VERSION }).context("Hello")?;

        let (tx, mut rx) = unbounded_channel::<FromHolder>();
        {
            let conn = conn.clone();
            std::thread::Builder::new()
                .name("holder-client".into())
                .spawn(move || {
                    loop {
                        match conn.recv::<FromHolder>() {
                            Ok(m) => {
                                if tx.send(m).is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("session holder connection ended: {e}");
                                return;
                            }
                        }
                    }
                })
                .context("spawning the holder reader thread")?;
        }

        // The first message is the answer to `Hello`. Re-queue it so the control loop starts
        // capture from it exactly as it does for a later generation.
        let hello = tokio::time::timeout(HELLO_TIMEOUT, rx.recv())
            .await
            .context("the session holder did not answer Hello")?
            .context("the session holder closed before answering Hello")?;
        let FromHolder::HelloOk { proto, generation, monitors } = hello else {
            bail!("the session holder answered Hello with {hello:?}");
        };
        if proto != PROTO_VERSION {
            return Ok(None);
        }
        tracing::info!(
            "session holder ready: protocol {proto}, generation {generation}, {} monitor(s)",
            monitors.len()
        );
        let (tx2, rx2) = unbounded_channel::<FromHolder>();
        let _ = tx2.send(FromHolder::HelloOk { proto, generation, monitors });
        let waiters: Waiters = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let w = waiters.clone();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                // A barrier acknowledgement belongs to whoever asked for it, not to the
                // control loop, so it is answered here and never forwarded.
                if let FromHolder::InputSynced { id } = m {
                    if let Some(tx) = w.lock().unwrap().remove(&id) {
                        let _ = tx.send(());
                    }
                    continue;
                }
                if tx2.send(m).is_err() {
                    return;
                }
            }
        });
        Ok(Some(Self {
            conn,
            inbox: std::sync::Mutex::new(Some(rx2)),
            fresh: false,
            waiters,
            next_sync: std::sync::atomic::AtomicU64::new(1),
        }))
    }

    /// Take the stream of holder messages. Called once, by the control loop.
    pub fn inbox(&self) -> UnboundedReceiver<FromHolder> {
        self.inbox.lock().unwrap().take().expect("the holder inbox is taken once")
    }

    /// Queue one message for the holder. Best-effort by design: the caller is either an
    /// input event, which is worthless once late, or a clipboard message the far end will
    /// retry.
    pub fn send(&self, msg: ToHolder) {
        if let Err(e) = self.conn.send(&msg) {
            tracing::warn!("sending to the session holder failed: {e:#}");
        }
    }

    /// Inject one input event.
    pub fn input(&self, msg: InputMsg) {
        self.send(ToHolder::Input(msg));
    }

    /// Wait until every input queued so far has actually been applied.
    ///
    /// [`Holder::input`] returns when the event is written to the socket, which says nothing
    /// about the pointer having moved: the holder injects serially and each notify is a
    /// D-Bus round trip, so a burst of moves queues far faster than it drains. A click that
    /// presses without waiting can therefore press against a position several events old,
    /// which is a miss on any target smaller than the error.
    ///
    /// Returns false if the holder did not answer within [`SYNC_TIMEOUT`].
    pub async fn sync_input(&self) -> bool {
        let id = self.next_sync.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiters.lock().unwrap().insert(id, tx);
        self.send(ToHolder::SyncInput { id });
        match tokio::time::timeout(SYNC_TIMEOUT, rx).await {
            Ok(Ok(())) => true,
            _ => {
                self.waiters.lock().unwrap().remove(&id);
                tracing::warn!("the session holder did not acknowledge input within {SYNC_TIMEOUT:?}");
                false
            }
        }
    }
}

/// Try the socket for [`CONNECT_TRIES`] attempts.
async fn dial(path: &str) -> Option<ipc::Conn> {
    for _ in 0..CONNECT_TRIES {
        match ipc::Conn::connect(path) {
            Ok(c) => return Some(c),
            Err(_) => tokio::time::sleep(CONNECT_WAIT).await,
        }
    }
    None
}

/// `systemctl --user <verb> rmng-session-holder`. Best-effort: a failure here shows up as
/// the dial that follows it failing, which is the error worth reporting.
async fn start_unit(verb: &str) {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", verb, "rmng-session-holder.service"])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => tracing::info!("systemctl --user {verb} rmng-session-holder"),
        Ok(o) => tracing::warn!(
            "systemctl --user {verb} rmng-session-holder failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => tracing::warn!("running systemctl: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::Listener;
    use wire::holder::HolderMonitor;

    fn temp_path(name: &str) -> String {
        let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        format!("{dir}/rmng-holder-test-{}-{name}.sock", std::process::id())
    }

    /// Answer one `Hello` with `proto`, then hold the connection open long enough for the
    /// client to finish reading.
    fn fake_holder(path: String, proto: u32, monitors: Vec<HolderMonitor>) {
        let listener = Listener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let c = listener.accept().unwrap();
            let _: ToHolder = c.recv().unwrap();
            c.send(&FromHolder::HelloOk { proto, generation: 4, monitors }).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
    }

    /// A holder left running by an older release answers with its own version, and the daemon
    /// has to notice. Missing it is worse than a reset: input and the clipboard would decode
    /// into `Unknown` on the far side and quietly do nothing.
    #[tokio::test]
    async fn a_version_mismatch_is_reported_rather_than_used() {
        let path = temp_path("mismatch");
        fake_holder(path.clone(), PROTO_VERSION + 1, vec![]);
        let conn = crate::ipc::Conn::connect(&path).unwrap();
        assert!(Holder::handshake(conn).await.unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// On a match the monitor set has to reach the control loop, which reads it from the
    /// inbox exactly like a later generation rather than through a separate path.
    #[tokio::test]
    async fn a_matching_version_queues_the_monitors_for_the_control_loop() {
        let path = temp_path("match");
        let mons = vec![HolderMonitor {
            monitor_id: 0,
            node_id: 51,
            width: 1920,
            height: 1080,
            x: 0,
            y: 0,
            primary: true,
        }];
        fake_holder(path.clone(), PROTO_VERSION, mons.clone());
        let conn = crate::ipc::Conn::connect(&path).unwrap();
        let holder = Holder::handshake(conn).await.unwrap().expect("versions match");
        let first = holder.inbox().recv().await.expect("the hello answer is re-queued");
        assert_eq!(
            first,
            FromHolder::HelloOk { proto: PROTO_VERSION, generation: 4, monitors: mons }
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Answer `Hello`, then answer every barrier, which is what a real holder does from its
    /// injection queue.
    fn fake_holder_answering_sync(path: String, monitors: Vec<HolderMonitor>) {
        let listener = Listener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let c = listener.accept().unwrap();
            let _: ToHolder = c.recv().unwrap();
            c.send(&FromHolder::HelloOk { proto: PROTO_VERSION, generation: 4, monitors }).unwrap();
            while let Ok(m) = c.recv::<ToHolder>() {
                if let ToHolder::SyncInput { id } = m {
                    c.send(&FromHolder::InputSynced { id }).unwrap();
                }
            }
        });
    }

    /// The barrier a click waits on has to complete, and it has to be answered to the caller
    /// rather than pushed at the control loop, which has no idea what to do with it.
    #[tokio::test]
    async fn a_barrier_completes_and_never_reaches_the_control_loop() {
        let path = temp_path("sync");
        fake_holder_answering_sync(path.clone(), vec![]);
        let conn = crate::ipc::Conn::connect(&path).unwrap();
        let holder = Holder::handshake(conn).await.unwrap().expect("versions match");
        holder.input(InputMsg::PointerMove { monitor_id: 0, x: 12.0, y: 34.0 });
        assert!(holder.sync_input().await, "the holder answered the barrier");

        let mut inbox = holder.inbox();
        assert!(matches!(inbox.recv().await, Some(FromHolder::HelloOk { .. })));
        assert!(
            inbox.try_recv().is_err(),
            "the acknowledgement belongs to the caller, not to the control loop"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A holder whose queue is wedged must not hang the click forever. Pressing a frame
    /// early beats never pressing at all.
    #[tokio::test]
    async fn a_barrier_gives_up_rather_than_hanging_the_click() {
        let path = temp_path("nosync");
        fake_holder(path.clone(), PROTO_VERSION, vec![]);
        let conn = crate::ipc::Conn::connect(&path).unwrap();
        let holder = Holder::handshake(conn).await.unwrap().expect("versions match");
        assert!(!holder.sync_input().await, "no answer means false, not a hang");
        let _ = std::fs::remove_file(&path);
    }
}
