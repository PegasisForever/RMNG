//! Keep every running managed clone's `/dev/shm` at LXC parity (~50% of the clone's RAM).
//!
//! Since the LXC→Docker port, clones inherit Docker's fixed **64 MB** `/dev/shm` default.
//! The old LXC clones booted systemd, which mounts `/dev/shm` as tmpfs at ~50% of the CT's
//! RAM (≈16 GB for a 32 GB clone). Chromium/Electron apps (Chrome, VSCode) keep renderer and
//! compositor bitmaps in POSIX shared memory under `/dev/shm`; when an allocation fails they
//! deliberately abort (`ud2` → SIGILL), so under real desktop load the 64 MB pool exhausts
//! and those processes drop.
//!
//! [`create_clone_container`](crate::docker::DockerCtl::create_clone_container) now sets
//! `shm_size` for clones created *after* the upgrade. This module covers the clones already
//! running: `ShmSize` can't be changed on a live container (`docker update` can't touch it)
//! and recreating would destroy the clone's writable layer, so the fix is a **live in-place
//! remount** of the tmpfs. The control-server is positioned to do it: its own container runs
//! `privileged: true` + `pid: "host"` and ships `/usr/bin/nsenter`, so it can enter each
//! clone's mount namespace by host PID and remount `/dev/shm` — the same mechanism used by
//! the clone-home reconciler ([`crate::homes`]).
//!
//! A 15s reconcile loop (matching the other reconcilers) checks each running managed clone's
//! current `/dev/shm` size by reading `/proc/<pid>/mountinfo` directly (no `nsenter` needed —
//! we're `pid: "host"`) and remounts only when it's below the clone's `Memory / 2` target.
//! The check is cheap and idempotent, so it's safe every tick — which is what makes the fix
//! durable: a live remount is lost whenever the container restarts (Docker re-creates
//! `/dev/shm` at 64 MB from the stored `HostConfig` on every start, including autonomous
//! `unless-stopped` restarts), and the loop re-applies it within one tick.

use std::collections::HashSet;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use wire::Host;

use crate::app::App;
use crate::files::is_safe_id;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

/// tmpfs stores its size as a page count, so a remount reads back rounded down to the page
/// size. The clone targets (`memory_mb·1MiB / 2`) are always page-aligned, so the read-back
/// equals the target exactly — but keep one page of slack in the idempotency check so a
/// hypothetical odd value can never trigger a re-remount every tick.
const PAGE_BYTES: u64 = 4096;

/// The outcome of reconciling one clone's `/dev/shm`. The loop uses it to warn once about a
/// missing `pid: "host"` instead of every tick.
enum Outcome {
    /// Remounted (or the remount attempt failed — already logged either way).
    Reconciled,
    /// Already at/above target, or not a running managed clone with a memory limit — nothing
    /// to do.
    Skipped,
    /// The clone's PID isn't visible in our `/proc` (operator forgot `pid: "host"`).
    ProcInvisible,
}

/// The `/dev/shm` tmpfs size in **bytes** parsed from a process's `/proc/<pid>/mountinfo`
/// content, or `None` if there's no `/dev/shm` tmpfs line or its `size=` is unparseable.
///
/// A mountinfo line is `<fields...> - <fstype> <source> <super-options>`; the mount point is
/// the 5th pre-separator field and tmpfs reports `size=<kib>k` in the super-options.
fn parse_shm_size(mountinfo: &str) -> Option<u64> {
    for line in mountinfo.lines() {
        let Some((fields, rest)) = line.split_once(" - ") else { continue };
        if fields.split_whitespace().nth(4) != Some("/dev/shm") {
            continue;
        }
        // Super-options are the last whitespace field after the ` - ` separator.
        let Some(super_opts) = rest.split_whitespace().last() else { continue };
        for opt in super_opts.split(',') {
            if let Some(val) = opt.strip_prefix("size=") {
                return parse_size_value(val);
            }
        }
    }
    None
}

/// Parse a tmpfs `size=` value into bytes. The kernel prints it in kibibytes with a `k`
/// suffix (e.g. `size=65536k` == 64 MiB); accept `k`/`m`/`g` (case-insensitive) and treat a
/// bare number as kibibytes to match that convention.
fn parse_size_value(val: &str) -> Option<u64> {
    let (num, mult) = match val.chars().last() {
        Some('k') | Some('K') => (&val[..val.len() - 1], 1024u64),
        Some('m') | Some('M') => (&val[..val.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&val[..val.len() - 1], 1024 * 1024 * 1024),
        _ => (val, 1024), // bare number → kibibytes (kernel's default unit)
    };
    num.parse::<u64>().ok().map(|n| n * mult)
}

/// Whether a remount is warranted: the current size is more than one page below target.
/// Pure so it's unit-testable. `saturating_sub` keeps an already-larger `/dev/shm` a no-op.
fn needs_remount(current: u64, target: u64) -> bool {
    target.saturating_sub(current) > PAGE_BYTES
}

/// Remount `/dev/shm` to `size` bytes inside the mount namespace of host PID `pid`, via
/// `nsenter` (the control-server is `privileged` + `pid: "host"` and ships nsenter). The
/// `nosuid,nodev,noexec` flags are passed explicitly — a bare `remount,size=…` silently drops
/// them, loosening the mount below what Docker created. `docker exec … mount -o remount` does
/// NOT work here (the mount point isn't in exec's namespace view); nsenter into the clone's
/// mount ns does.
async fn remount_shm(pid: i64, size: u64) -> std::io::Result<std::process::ExitStatus> {
    Command::new("nsenter")
        .args([
            "-t".to_string(),
            pid.to_string(),
            "-m".to_string(),
            "mount".to_string(),
            "-o".to_string(),
            format!("remount,size={size},nosuid,nodev,noexec"),
            "/dev/shm".to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map(|o| {
            if !o.status.success() {
                let err = String::from_utf8_lossy(&o.stderr);
                tracing::warn!(target: "shm", "nsenter remount (pid {pid}) failed: {}", err.trim());
            }
            o.status
        })
}

/// Reconcile one clone's `/dev/shm` up to `Memory / 2`. Best-effort: a transient inspect
/// failure returns `Skipped` and the next tick retries. Logs once (info) when it actually
/// remounts — the idempotency check keeps every other tick silent.
async fn ensure_for(app: &App, id: &str) -> Outcome {
    let (pid, mem) = match app.docker.container_pid_and_memory(id).await {
        Ok(Some(pm)) => pm,
        Ok(None) => return Outcome::Skipped, // stopped / gone / unlimited memory
        Err(e) => {
            tracing::debug!(target: "shm", "inspect for {id} failed: {e:#}");
            return Outcome::Skipped;
        }
    };
    let target = (mem / 2) as u64;

    // Read the current size from OUR view of the clone's mountinfo — `pid: "host"` makes
    // /proc/<pid> the clone's, so this needs no nsenter and doubles as the pid-visibility probe.
    let mountinfo = match std::fs::read_to_string(format!("/proc/{pid}/mountinfo")) {
        Ok(s) => s,
        // ENOENT on /proc/<pid> ⇒ the clone's PID isn't in our namespace (`pid: "host"`
        // missing). Any other read error is transient → treat as skip and retry next tick.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Outcome::ProcInvisible,
        Err(e) => {
            tracing::debug!(target: "shm", "reading /proc/{pid}/mountinfo for {id}: {e}");
            return Outcome::Skipped;
        }
    };

    let current = parse_shm_size(&mountinfo).unwrap_or(0);
    if !needs_remount(current, target) {
        return Outcome::Skipped;
    }

    match remount_shm(pid, target).await {
        Ok(st) if st.success() => {
            tracing::info!(
                target: "shm",
                "resized {id} /dev/shm {}MiB → {}MiB (pid {pid})",
                current / (1024 * 1024),
                target / (1024 * 1024),
            );
        }
        Ok(_) => {} // remount_shm already logged the stderr
        Err(e) => tracing::warn!(target: "shm", "spawning nsenter for {id} (pid {pid}): {e}"),
    }
    Outcome::Reconciled
}

/// One reconcile pass over every running managed clone. `warned` tracks host ids we've already
/// logged a missing-`pid: "host"` warning for, so the hint fires once, not every tick.
async fn reconcile(app: &App, warned: &mut HashSet<String>) {
    let hosts: Vec<Host> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived && is_safe_id(&h.id))
        .collect();

    for h in &hosts {
        match ensure_for(app, &h.id).await {
            Outcome::ProcInvisible => {
                if warned.insert(h.id.clone()) {
                    tracing::warn!(
                        target: "shm",
                        "clone {} pid not visible in /proc — add `pid: \"host\"` to the \
                         control-server service (compose.yaml) to reconcile clone /dev/shm size",
                        h.id
                    );
                }
            }
            _ => {
                warned.remove(&h.id); // resolved → allow a fresh warning if it ever recurs
            }
        }
    }

    // Keep the once-warned set bounded to hosts that still exist + are managed.
    let managed: HashSet<String> = hosts.iter().map(|h| h.id.clone()).collect();
    warned.retain(|id| managed.contains(id));
}

/// Reconcile a single clone's `/dev/shm` immediately (best-effort, fire-and-forget). Called
/// right after a control-server-initiated clone start so the desktop gets its full `/dev/shm`
/// without waiting up to one reconcile tick. The loop catches it either way.
pub async fn ensure_now(app: &App, id: &str) {
    let _ = ensure_for(app, id).await;
}

/// Background reconcile loop; spawned once at startup (matches [`crate::homes::run`]).
pub async fn run(app: App) {
    tracing::info!("/dev/shm reconciler started (LXC parity, every {}s)", RECONCILE_INTERVAL.as_secs());
    let mut warned: HashSet<String> = HashSet::new();
    loop {
        reconcile(&app, &mut warned).await;
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHM_64M: &str = "\
843 771 0:58 / /etc/hosts rw,relatime shared:412 - ext4 /dev/sda1 rw
845 771 0:60 / /dev/shm rw,nosuid,nodev,noexec,relatime shared:414 - tmpfs shm rw,size=65536k,inode64
846 771 0:61 / /proc rw,relatime - proc proc rw";

    #[test]
    fn parse_shm_size_reads_the_tmpfs_line() {
        assert_eq!(parse_shm_size(SHM_64M), Some(64 * 1024 * 1024));
    }

    #[test]
    fn parse_shm_size_none_when_absent() {
        let no_shm = "845 771 0:60 / /proc rw,relatime - proc proc rw";
        assert_eq!(parse_shm_size(no_shm), None);
    }

    #[test]
    fn parse_shm_size_16g() {
        let line = "845 771 0:60 / /dev/shm rw,nosuid,nodev,noexec - tmpfs shm rw,size=16777216k,inode64";
        assert_eq!(parse_shm_size(line), Some(16 * 1024 * 1024 * 1024));
    }

    #[test]
    fn parse_shm_size_ignores_a_similarly_named_mount() {
        // A mount whose point merely contains "shm" must not be mistaken for /dev/shm.
        let line = "845 771 0:60 / /run/shm.d rw - tmpfs t rw,size=8k";
        assert_eq!(parse_shm_size(line), None);
    }

    #[test]
    fn parse_size_value_units() {
        assert_eq!(parse_size_value("65536k"), Some(64 * 1024 * 1024));
        assert_eq!(parse_size_value("512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_size_value("2G"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size_value("1024"), Some(1024 * 1024)); // bare → kibibytes
        assert_eq!(parse_size_value("nope"), None);
    }

    #[test]
    fn needs_remount_true_when_far_below_target() {
        let target = 16 * 1024 * 1024 * 1024;
        assert!(needs_remount(64 * 1024 * 1024, target)); // 64M default vs 16G target
    }

    #[test]
    fn needs_remount_false_when_at_or_above_target() {
        let target = 16 * 1024 * 1024 * 1024;
        assert!(!needs_remount(target, target));
        assert!(!needs_remount(target + 1, target)); // already larger
        assert!(!needs_remount(target - PAGE_BYTES, target)); // within one page → no churn
    }
}
