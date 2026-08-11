//! The daemon's handle on the session the holder owns.
//!
//! Everything Mutter will only answer for the connection that created the session (input
//! injection, the clipboard, the monitors themselves) goes through here as a message. The
//! one D-Bus connection the daemon keeps for itself is the session bus, for
//! `org.gnome.Shell.Eval`: the window tools are a plain gnome-shell method with no
//! per-connection check, so they need no holder round trip.

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
}

impl Holder {
    /// Connect to the holder, starting it if it is not running and restarting it if it
    /// speaks a different protocol version.
    ///
    /// A version mismatch costs one window reset on that release, which is why the version
    /// is bumped only for a real protocol change. Every other release reconnects to the
    /// holder that was already running, and the clone's windows never move.
    pub async fn connect() -> Result<Self> {
        let path = wire::holder::socket_path();
        let conn = match dial(&path).await {
            Some(c) => c,
            None => {
                tracing::warn!("no session holder at {path}; starting the unit");
                start_unit("start").await;
                dial(&path)
                    .await
                    .with_context(|| format!("no session holder at {path} after starting it"))?
            }
        };

        let holder = Self::handshake(conn).await?;
        let Some(holder) = holder else {
            tracing::warn!(
                "the session holder speaks a different protocol version; restarting it. \
                 This release resets window positions once."
            );
            start_unit("restart").await;
            let conn = dial(&path)
                .await
                .with_context(|| format!("no session holder at {path} after restarting it"))?;
            return Self::handshake(conn)
                .await?
                .context("the restarted session holder still speaks a different protocol");
        };
        Ok(holder)
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
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if tx2.send(m).is_err() {
                    return;
                }
            }
        });
        Ok(Some(Self { conn, inbox: std::sync::Mutex::new(Some(rx2)) }))
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
}
