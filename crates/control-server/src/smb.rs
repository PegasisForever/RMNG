//! `smbd` supervisor + the `clones` SMB share. The control-server runs its own smbd
//! (port 445) exporting one read-write share whose root is `data/hosts` — the symlink
//! directory the clone-home reconciler (`homes.rs`) maintains, one link per running clone
//! pointing at that clone's `/home/rmng`. So an SMB client browsing `\\<host>\clones` sees
//! every clone's home side by side.
//!
//! `force user = rmng` (uid 1000) makes smbd both *traverse* those `/proc/<pid>/root`
//! symlinks — a uid-1000 session can only ptrace-follow a fellow uid-1000 process, which
//! is exactly why `homes` links through a uid-1000 pid — and *own* newly created files as
//! uid 1000 inside the clone.
//!
//! On startup we (re)render `/data/smb.conf` from the live config's `data_dir` (so the
//! share `path` always tracks where the reconciler writes its links), provision the local
//! `rmng` account + its SMB password (idempotent — safe on every boot), then supervise
//! `smbd --foreground -s /data/smb.conf` forever with capped-backoff restarts. Always-on
//! and harmless without `pid: "host"`: `data/hosts` is simply empty, so the share is too.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::app::App;
use crate::homes;

/// Where smbd reads its config, and where we render it. Fixed (the WORKDIR is `/data`), so
/// the daemon invocation stays a bare `-s /data/smb.conf` with no config surface.
const SMB_CONF: &str = "/data/smb.conf";

/// Restart backoff: first retry after `BASE`, doubling per consecutive quick crash up to
/// `MAX`. A run that stays up past `STABLE` resets the counter (see [`supervise`]).
const BASE_BACKOFF: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const STABLE_RUN: Duration = Duration::from_secs(60);

/// Render `smb.conf` for the `clones` share rooted at `hosts_root`. Pure (no I/O) so it's
/// unit-testable. Only `path` varies; every other line is literal per the design. The
/// section keys are indented for readability — smbd ignores leading whitespace.
pub fn render_smb_conf(hosts_root: &Path) -> String {
    format!(
        "[global]
   server min protocol = SMB2
   unix extensions = no
   allow insecure wide links = yes
   security = user
   smb ports = 445
   load printers = no
   printing = bsd
   disable spoolss = yes
   vfs objects = catia fruit streams_xattr
   log level = 1

[clones]
   path = {}
   read only = no
   wide links = yes
   follow symlinks = yes
   force user = rmng
   force group = rmng
   valid users = rmng
",
        hosts_root.display()
    )
}

/// The share root as an absolute path. `homes::hosts_root` is `data/hosts` relative to the
/// WORKDIR (`/data` → `/data/data/hosts`); smb.conf needs it absolute. Lexical only (no
/// symlink resolution). Sourced from `homes` so the reconciler and the share never diverge.
fn absolute_hosts_root(data_dir: &str) -> PathBuf {
    let root = homes::hosts_root(data_dir);
    std::path::absolute(&root).unwrap_or(root)
}

/// Delay before the next smbd restart given the number of consecutive quick failures.
/// `BASE * 2^failures`, capped at `MAX`. Pure + saturating throughout, so a runaway crash
/// loop can never overflow the multiply — it just pins at `MAX`.
fn backoff(failures: u32) -> Duration {
    BASE_BACKOFF.saturating_mul(2u32.saturating_pow(failures)).min(MAX_BACKOFF)
}

/// Ensure the local `rmng` account + its SMB password exist. Idempotent and best-effort:
/// in a built image (Task 3) the account is already present, so every step here is expected
/// to be a no-op / "already exists" and its failure is ignored. Provided only so a dev box
/// (where samba may be absent) doesn't hard-fail. All child output is discarded to keep the
/// server log clean.
async fn provision_account() {
    // Dev fallback: create the account if missing. In the image it exists (these fail with
    // "already exists"); in dev without root they fail on permission — both ignored.
    let _ = Command::new("groupadd")
        .args(["-g", "1000", "rmng"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    let _ = Command::new("useradd")
        .args(["-u", "1000", "-g", "1000", "-M", "-s", "/usr/sbin/nologin", "rmng"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    // Set the SMB password to the literal `rmng`. `smbpasswd -a -s` reads the new password
    // twice from stdin; feeding it here makes this non-interactive and re-runnable.
    match Command::new("smbpasswd")
        .args(["-a", "-s", "rmng"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"rmng\nrmng\n").await;
                drop(stdin); // close the pipe so smbpasswd sees EOF and exits
            }
            let _ = child.wait().await;
        }
        // No samba in dev → nothing to set; the supervisor logs the real error when it
        // tries to spawn smbd. Keep this quiet (debug) so a dev boot isn't noisy.
        Err(e) => tracing::debug!(target: "smb", "smbpasswd unavailable (samba not installed?): {e}"),
    }
}

/// Spawn `smbd` in the foreground with its stdout/stderr piped for line logging.
/// `--debug-stdout` routes smbd's debug/error output to stdout (otherwise it goes to the
/// logfile), so the piped forwarding below actually sees failures and the supervisor isn't
/// blind.
fn spawn_smbd() -> std::io::Result<Child> {
    Command::new("smbd")
        .args(["--foreground", "--no-process-group", "--debug-stdout", "-s", SMB_CONF])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Forward every line from a child pipe to `tracing` at info on the `smb` target. A read
/// `Err` (e.g. a non-UTF-8 byte) must NOT stop draining — that could let the sibling pipe
/// fill its 64KB buffer and wedge smbd — so keep reading past it; only EOF (`Ok(None)`)
/// ends the loop.
async fn log_lines<R: AsyncRead + Unpin>(reader: R) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => tracing::info!(target: "smb", "{line}"),
            Ok(None) => break,
            Err(_) => continue,
        }
    }
}

/// Run one smbd instance to completion, streaming its stdout+stderr line-by-line. Returns
/// when the process exits (drains both pipes concurrently with the wait so a chatty smbd
/// can't deadlock on a full pipe).
async fn run_smbd(mut child: Child) {
    let out = child.stdout.take();
    let err = child.stderr.take();
    let stream_out = async {
        if let Some(r) = out {
            log_lines(r).await;
        }
    };
    let stream_err = async {
        if let Some(r) = err {
            log_lines(r).await;
        }
    };
    let (status, (), ()) = tokio::join!(child.wait(), stream_out, stream_err);
    match status {
        Ok(s) => tracing::warn!(target: "smb", "smbd exited ({s}) — restarting"),
        Err(e) => tracing::warn!(target: "smb", "waiting on smbd failed: {e}"),
    }
}

/// Supervise smbd forever: (re)spawn, stream its logs, and restart on exit with capped
/// backoff. Never returns and never panics — a permanently-broken smbd (e.g. samba not
/// installed) just retries at the `MAX` cadence without taking the server down. The first
/// spawn/exec failure is logged at error once; repeats drop to debug so the log doesn't
/// fill with the same line every 5 minutes.
async fn supervise() {
    let mut failures: u32 = 0;
    let mut spawn_error_logged = false;
    loop {
        let started = Instant::now();
        match spawn_smbd() {
            Ok(child) => {
                spawn_error_logged = false; // a successful spawn re-arms the one-shot error
                run_smbd(child).await;
            }
            Err(e) if !spawn_error_logged => {
                tracing::error!(target: "smb", "failed to spawn smbd (is samba installed?): {e}");
                spawn_error_logged = true;
            }
            Err(e) => tracing::debug!(target: "smb", "smbd spawn still failing: {e}"),
        }
        // A run that stayed up long enough is a healthy restart, not a crash loop → reset,
        // so the next backoff is BASE again rather than an ever-growing delay.
        if started.elapsed() >= STABLE_RUN {
            failures = 0;
        }
        let delay = backoff(failures);
        failures = failures.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
}

/// Render the config, provision the account, then supervise smbd forever. Spawned once at
/// startup from `main` alongside `homes::run`.
pub async fn run(app: App) {
    let cfg = app.config();
    let root = absolute_hosts_root(&cfg.data_dir);
    let _ = std::fs::create_dir_all(&root); // harmless if homes already made it

    match std::fs::write(SMB_CONF, render_smb_conf(&root)) {
        Ok(()) => tracing::info!(target: "smb", "wrote {SMB_CONF} (share root {})", root.display()),
        Err(e) => tracing::error!(target: "smb", "writing {SMB_CONF}: {e}"),
    }

    provision_account().await;
    supervise().await; // never returns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_smb_conf_has_load_bearing_lines() {
        let out = render_smb_conf(Path::new("/data/data/hosts"));
        for needle in [
            "[global]",
            "server min protocol = SMB2",
            "unix extensions = no",
            "smb ports = 445",
            "[clones]",
            "path = /data/data/hosts",
            "read only = no",
            "wide links = yes",
            "follow symlinks = yes",
            "force user = rmng",
            "force group = rmng",
            "valid users = rmng",
        ] {
            assert!(out.contains(needle), "smb.conf missing `{needle}`:\n{out}");
        }
    }

    #[test]
    fn render_smb_conf_interpolates_the_share_path() {
        // The share root must be exactly where the reconciler links, else the share is
        // silently empty — so `path` tracks the argument, not a hardcoded default.
        let out = render_smb_conf(Path::new("/srv/rmng/data/hosts"));
        assert!(out.contains("path = /srv/rmng/data/hosts"), "{out}");
        assert!(!out.contains("/data/data/hosts"), "{out}");
    }

    #[test]
    fn backoff_escalates_then_caps() {
        assert_eq!(backoff(0), Duration::from_secs(30));
        assert_eq!(backoff(1), Duration::from_secs(60));
        assert_eq!(backoff(3), Duration::from_secs(240));
        // Capped at 300s from failure 4 on…
        assert_eq!(backoff(4), Duration::from_secs(300));
        assert_eq!(backoff(10), Duration::from_secs(300));
        // …and saturating, so even an absurd count can't overflow the multiply.
        assert_eq!(backoff(u32::MAX), Duration::from_secs(300));
    }
}
