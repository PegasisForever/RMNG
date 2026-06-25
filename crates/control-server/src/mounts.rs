//! `data/hosts/<id>` — each host's home, FUSE-mounted over SFTP so every host's
//! files are browsable on the control-server box (and so the Claude importer can
//! read the template host's claude-swap dir). Port of `mounts.server.ts`: a 15s
//! reconcile loop sshfs-mounts new hosts, re-mounts dead ones, and tears down
//! directories for removed hosts. Best-effort — a down host just retries next tick.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use wire::Host;

use crate::app::App;
use crate::files::is_safe_id;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

fn hosts_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("hosts")
}

/// `/proc/mounts` octal-escape decode (`\040` → space, …).
fn unescape_mount(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let n = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(n as char);
            i += 4;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// Absolute mount points currently served by a FUSE filesystem (i.e. sshfs).
fn fuse_mounts() -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(s) = std::fs::read_to_string("/proc/mounts") {
        for line in s.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            if f.len() >= 3 && f[2].starts_with("fuse") {
                set.insert(unescape_mount(f[1]));
            }
        }
    }
    set
}

/// True if the mount root answers a stat within the timeout (i.e. it's live).
async fn mount_responsive(mp: &Path) -> bool {
    let mp = mp.to_path_buf();
    let stat = tokio::task::spawn_blocking(move || std::fs::metadata(&mp).is_ok());
    matches!(tokio::time::timeout(HEALTH_TIMEOUT, stat).await, Ok(Ok(true)))
}

/// `fusermount(3) -u [-z]`; tries fusermount3 then fusermount.
async fn unmount(mp: &Path, lazy: bool) {
    for bin in ["fusermount3", "fusermount"] {
        let mut args = vec!["-u"];
        if lazy {
            args.push("-z");
        }
        let mp_s = mp.to_string_lossy().to_string();
        args.push(&mp_s);
        match tokio::process::Command::new(bin).args(&args).output().await {
            Ok(_) => return,                                    // ran (success or not)
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // try the other bin
            Err(_) => return,
        }
    }
}

struct SshfsMissing;

/// Mount host's home over SFTP at `data/hosts/<id>`.
async fn mount_sshfs(host: &Host, mp: &Path) -> Result<(), Result<String, SshfsMissing>> {
    let mut opts = vec![
        "reconnect".to_string(),
        "ServerAliveInterval=15".into(),
        "ServerAliveCountMax=3".into(),
        "ConnectTimeout=10".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];
    let use_password = !host.password.is_empty();
    opts.push(if use_password { "password_stdin".into() } else { "BatchMode=yes".into() });
    let target = format!("{}@{}:", host.username, host.host); // empty path = remote home
    let mp_s = mp.to_string_lossy();
    let opts_s = opts.join(",");

    let mut child = match tokio::process::Command::new("sshfs")
        .args([target.as_str(), mp_s.as_ref(), "-o", opts_s.as_str()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Err(SshfsMissing)),
        Err(e) => return Err(Ok(e.to_string())),
    };
    if use_password {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(format!("{}\n", host.password).as_bytes()).await;
        }
    }
    let out = child.wait_with_output().await.map_err(|e| Ok(e.to_string()))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let mut tail: Vec<&str> = err.lines().rev().take(3).collect();
        tail.reverse();
        Err(Ok(tail.join("; ")))
    }
}

async fn reconcile(app: &App) {
    let cfg = app.config();
    let root = hosts_root(&cfg.data_dir);
    let _ = std::fs::create_dir_all(&root);

    let hosts: Vec<Host> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| is_safe_id(&h.id) && !h.host.is_empty() && !h.username.is_empty())
        .collect();
    let desired: HashSet<String> = hosts.iter().map(|h| h.id.clone()).collect();
    let mounted = fuse_mounts();

    for h in &hosts {
        let mp = root.join(&h.id);
        let _ = std::fs::create_dir_all(&mp);
        let mp_str = mp.to_string_lossy().to_string();
        if mounted.contains(&mp_str) {
            if mount_responsive(&mp).await {
                continue; // healthy
            }
            unmount(&mp, true).await; // stale/hung → drop, re-mount below
        }
        match mount_sshfs(h, &mp).await {
            Ok(()) => tracing::info!("mounted {} → {}@{}:", h.id, h.username, h.host),
            Err(Err(SshfsMissing)) => {
                tracing::warn!("host mounts disabled: `sshfs` is not installed");
                return; // no point trying other hosts this tick
            }
            Err(Ok(msg)) => tracing::warn!("mount {} failed: {msg}", h.id),
        }
    }

    // Tear down directories for hosts that no longer exist.
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if desired.contains(&name) {
                continue;
            }
            let mp = root.join(&name);
            unmount(&mp, true).await;
            if std::fs::remove_dir(&mp).is_ok() {
                tracing::info!("unmounted removed host {name}");
            }
        }
    }
}

/// Background reconcile loop; spawned once at startup.
pub async fn run(app: App) {
    tracing::info!("host mount reconciler started (every {}s)", RECONCILE_INTERVAL.as_secs());
    loop {
        reconcile(&app).await;
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}
