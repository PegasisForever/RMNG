//! `data/hosts/<id>` — every managed clone's home (`/home/rmng`) surfaced in one place, so
//! all clones' files are browsable from a single directory (on the control-server box, and
//! on the Docker host itself via the `rmng-data` volume at
//! `/var/lib/docker/volumes/rmng-data/_data/hosts/…`).
//!
//! The Docker-port successor to the Proxmox-era sshfs reconciler (`mounts.rs`, deleted):
//! instead of FUSE-mounting each clone's home over SSH, it maintains a plain symlink
//! `<data_dir>/hosts/<id>` → `/proc/<uid-1000-pid>/root/home/rmng` for every RUNNING managed
//! clone. The target is a uid-1000 process's proc-root (not the clone's root-owned init) so
//! the SMB share (smb.rs, smbd acting as uid 1000) can follow the link; every process in the
//! clone shares one rootfs, so host-side / `docker exec` browsing is unaffected.
//! This works because the control-server container runs with `pid: "host"` (see
//! compose.yaml): it shares the Docker host's PID namespace, so `/proc/<pid>/root/...` IS
//! the clone container's root filesystem — and the very same link path resolves on the
//! host too (that's the user's access path).
//!
//! A 15s reconcile loop (same cadence as the old one) links new/running clones, repoints
//! stale links (a clone's PID changes across restarts), and removes links for
//! stopped/deleted/unmanaged clones. Best-effort throughout: a transient daemon error just
//! retries next tick. When a clone's PID is known but `/proc/<pid>` isn't visible in our
//! namespace (operator forgot `pid: "host"`), it warns ONCE per host, then skips.
//!
//! Not every uid-1000 process will do. A sandboxed browser tab chroots itself out of the clone's
//! filesystem, and linking one produces a path that never resolves; [`pick_home_pid`] is where
//! that is rejected, and it says what breaks when it is not. Three separate features read the
//! clone's files through this link, so a bad pick is not only a browsing problem.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use wire::RmngClone;

use crate::app::App;
use crate::docker::CLONE_USER;
use crate::files::is_safe_id;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

/// How long [`ensure_now`] waits for a newly-created clone to open a uid-1000 session, and how
/// often it re-checks. A clone whose daemon has registered already has one, so the common case
/// costs a single probe.
const SESSION_WAIT: Duration = Duration::from_secs(10);
const SESSION_POLL: Duration = Duration::from_millis(500);

/// The clone user's uid (see `docker::CLONE_USER`). The SMB share acts as this uid, so the
/// browse link must point at a uid-1000 process's proc-root.
const CLONE_UID: u32 = 1000;

/// The directory holding one symlink per managed clone home (`<data_dir>/hosts`).
/// `pub(crate)` so smb.rs single-sources the SMB share `path` from it (the share root is
/// exactly where the reconciler writes links, so the two can never diverge).
pub(crate) fn hosts_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("hosts")
}

/// The clone's home as seen through the shared PID namespace: with `pid: "host"`,
/// `/proc/<pid>/root` is the clone container's root fs, so the rmng user's home lives here.
fn clone_home(pid: i64) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/root/home/{CLONE_USER}"))
}

/// The absolute path prefix a clone-side path has to sit under for the host to reach it
/// through these links. The link stands in for `/home/rmng`, so nothing above it is
/// addressable this way.
const CLONE_HOME_PREFIX: &str = "/home/rmng";

/// True when a clone-side path is the agent's home itself rather than something inside it.
///
/// Deleting to match a source there would take `Desktop`, `.ssh`, `.claude` and the agent's
/// own state with it, so the sync form refuses.
pub(crate) fn is_home_root(clone_path: &str) -> bool {
    clone_path.trim_end_matches('/') == CLONE_HOME_PREFIX
}

/// Map an absolute clone-side path onto the host, through the clone's browse link.
///
/// `None` for anything outside the clone's home, and for any `..` component, so a caller
/// cannot walk out of the link into the control-server's own filesystem.
pub(crate) fn host_path(data_dir: &str, clone: &str, clone_path: &str) -> Option<PathBuf> {
    if !is_safe_id(clone) {
        return None;
    }
    let rel = clone_path
        .strip_prefix(CLONE_HOME_PREFIX)
        .map(|r| r.trim_start_matches('/'))?;
    if Path::new(rel).components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return None;
    }
    let mut p = hosts_root(data_dir).join(clone);
    if !rel.is_empty() {
        p.push(rel);
    }
    Some(p)
}

/// Copy a directory from one clone's home into another's, entirely on this host.
///
/// Both ends are reached through the browse links, so the bytes never enter a container,
/// the Docker API, or a socket: this is one `cp` between two directories the control-server
/// can already see. That is what makes it the right path for a large tree, where the
/// streaming upload would move every byte twice more.
///
/// `rclone` does the work, because a project tree is thousands of small files and it copies
/// them in parallel: 2.1s against 5.8s for `cp -a` on a 23,470-file tree. `cp -a` remains
/// the fallback, and is the faster of the two on a handful of huge files, where there is no
/// file-level parallelism to exploit and its sequential I/O runs at roughly 8 GB/s.
///
/// Excludes name a directory at the top of the copied tree and nothing deeper, so a
/// top-level `dist/` can be skipped while every `node_modules/*/dist` still crosses.
///
/// Returns the bytes now sitting at the destination.
///
/// `delete` makes the destination match instead of merge, removing what the source does not
/// have. An excluded name is left alone rather than deleted, so a subclone keeps its own
/// `target/` through a sync that excludes it.
pub async fn copy_between(
    app: &App,
    from: &str,
    from_path: &str,
    to: &str,
    to_path: &str,
    excludes: &[String],
    delete: bool,
) -> anyhow::Result<u64> {
    // A clone created moments ago may not be linked yet; this is what waits for it.
    ensure_now(app, from).await;
    ensure_now(app, to).await;

    let data_dir = app.config().data_dir;
    let src = host_path(&data_dir, from, from_path)
        .ok_or_else(|| anyhow::anyhow!("{from}:{from_path} is not inside {CLONE_HOME_PREFIX}"))?;
    let dst = host_path(&data_dir, to, to_path)
        .ok_or_else(|| anyhow::anyhow!("{to}:{to_path} is not inside {CLONE_HOME_PREFIX}"))?;
    // Syncing onto the home itself would delete Desktop, .ssh, .claude and the agent's own
    // state along with everything else the source happens not to have. A merge there is
    // harmless, so the guard is only on the deleting form.
    if delete && dst == hosts_root(&data_dir).join(to) {
        anyhow::bail!(
            "refusing to sync onto {to}:{CLONE_HOME_PREFIX} itself; name a directory inside it"
        );
    }
    let skip: HashSet<String> = excludes.iter().cloned().collect();

    tokio::task::spawn_blocking(move || {
        if !src.is_dir() {
            anyhow::bail!("{} is not a directory on the source clone", src.display());
        }
        std::fs::create_dir_all(&dst)?;
        // The destination directory itself belongs to whoever owns the source, so the agent
        // can write in its own project rather than finding it owned by root.
        if let Ok(meta) = std::fs::metadata(&src) {
            use std::os::unix::fs::MetadataExt;
            let _ = std::os::unix::fs::chown(&dst, Some(meta.uid()), Some(meta.gid()));
        }
        if which_rclone() {
            run_rclone(&src, &dst, &skip, delete)?;
            mirror_dir_owners(&src, &dst);
        } else {
            if delete {
                prune_extraneous(&src, &dst, &skip);
            }
            run_cp(&src, &dst, &skip)?;
        }
        Ok(dir_bytes(&dst))
    })
    .await?
}

fn which_rclone() -> bool {
    Path::new("/usr/bin/rclone").exists()
}

/// Copy with rclone, parallel across files.
///
/// Two flags are load-bearing rather than optional. Without `--metadata` every file lands
/// owned by root, since this process is root, and the agent cannot write its own project.
/// Without `--links` symlinks are dropped silently: a tree with 32 of them arrived with
/// none. `--ignore-checksum` skips a verification pass that would re-read both sides.
fn run_rclone(
    src: &Path,
    dst: &Path,
    skip: &HashSet<String>,
    delete: bool,
) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new("/usr/bin/rclone");
    // `sync` makes the destination match; `copy` only adds and overwrites. Filters apply to
    // both sides, so an excluded name is neither copied nor deleted.
    cmd.arg(if delete { "sync" } else { "copy" })
        .arg("--metadata")
        .arg("--links")
        .arg("--ignore-checksum")
        .arg("--transfers")
        .arg("32")
        .arg("--checkers")
        .arg("32")
        // rclone reads a config it does not need here, and warns when there is none.
        .arg("--config")
        .arg("/dev/null");
    for name in skip {
        // A leading slash anchors to the root of the transfer, so this is the top-level
        // directory of that name and no other.
        cmd.arg("--exclude").arg(format!("/{name}/**"));
    }
    let out = cmd.arg(src).arg(dst).output()?;
    if !out.status.success() {
        anyhow::bail!("rclone failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Delete what the destination has and the source does not, for the `cp -a` fallback.
///
/// An excluded top-level name is left alone, matching what rclone's filters do on the other
/// path: a subclone keeps its own `target/` through a sync that excludes it.
fn prune_extraneous(src: &Path, dst: &Path, skip: &HashSet<String>) {
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dst.join(&rel)) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = rel.join(entry.file_name());
            let top = child.components().next().map(|c| c.as_os_str().to_string_lossy().to_string());
            if top.is_some_and(|t| skip.contains(&t)) {
                continue;
            }
            if std::fs::symlink_metadata(src.join(&child)).is_err() {
                let path = dst.join(&child);
                let _ = if path.is_dir() && !path.is_symlink() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
            } else if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(child);
            }
        }
    }
}

/// Give every copied directory the owner and mode its source has.
///
/// `--metadata` covers files and stops there: rclone 1.60, which Ubuntu ships, has no
/// directory metadata. Left alone, a seeded tree arrives with every directory owned by root,
/// because this process is root. The agent can then read and even edit files, since those
/// carry the right owner, but it cannot create one anywhere in its own project. A 23,470
/// file tree landed with 3,647 of its 3,648 directories unwritable that way.
///
/// Best-effort per entry: one unreadable directory is not a reason to fail a copy that has
/// already happened.
fn mirror_dir_owners(src: &Path, dst: &Path) {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let (s, d) = (src.join(&rel), dst.join(&rel));
        let Ok(meta) = std::fs::symlink_metadata(&s) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let _ = std::os::unix::fs::chown(&d, Some(meta.uid()), Some(meta.gid()));
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(meta.mode()));
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(rel.join(entry.file_name()));
            }
        }
    }
}

/// The fallback when the image has no rclone. Slower on a source tree, faster on a few
/// large files, and the only one of the two that keeps hardlinks as hardlinks.
///
/// Excluding is structural here: the top level is enumerated and filtered, and what survives
/// goes to a single `cp -a` so hardlinks between those entries are preserved.
fn run_cp(src: &Path, dst: &Path, skip: &HashSet<String>) -> anyhow::Result<()> {
    let mut sources: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if skip.contains(entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        sources.push(entry.path());
    }
    if sources.is_empty() {
        return Ok(());
    }
    let out = std::process::Command::new("cp")
        .arg("-a")
        .arg("--")
        .args(&sources)
        .arg(dst)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("cp -a failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Apparent size of everything under `dir`, following no symlinks.
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// The `/proc/<pid>` entry whose presence proves the clone's PID is visible in our
/// namespace (i.e. the operator did add `pid: "host"`).
fn proc_dir(pid: i64) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}"))
}

/// From `(pid, uid, mnt_ns_ino)` triples, pick a pid in the clone's mount namespace
/// (`target_mnt_ino`) that runs as the clone user AND whose proc-root leads to the clone's home.
///
/// `usable` is that last test, injected so this stays pure and unit-testable. It is not a
/// formality: **a sandboxed browser tab passes every other check and fails this one.** Chrome and
/// Firefox renderers `chroot` themselves into `/proc/<pid>/fdinfo`, so they run as uid 1000, sit
/// in the clone's mount namespace, and report a `/proc/<pid>/root` that leads nowhere near the
/// clone's filesystem. Once that pid is picked the link is broken for as long as the tab lives:
/// the pid never changes, so [`ensure_symlink`] sees a link that already matches and leaves it,
/// and the clone loses home browsing, token counting, and log-based activity detection all at
/// once. Six of thirty-six clones on CT 105 were in that state, each pinned to a `Web Content` or
/// `Isolated Web Co` process whose root pointed at an `fdinfo` directory of a long-dead pid.
///
/// Candidates are tried lowest pid first: within one clone that is the earliest-started process,
/// which is the session leader rather than something transient a turn spawned.
fn pick_home_pid(
    target_mnt_ino: u64,
    candidates: &[(i64, u32, u64)],
    usable: impl Fn(i64) -> bool,
) -> Option<i64> {
    let mut pids: Vec<i64> = candidates
        .iter()
        .filter(|(_, uid, ino)| *uid == CLONE_UID && *ino == target_mnt_ino)
        .map(|(pid, _, _)| *pid)
        .collect();
    pids.sort_unstable();
    pids.into_iter().find(|pid| usable(*pid))
}

/// Real uid from `/proc/<pid>/status` (first field of the `Uid:` line).
fn proc_uid(pid: i64) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Inode of `/proc/<pid>/ns/mnt` — identical for every process in one mount namespace
/// (i.e. one clone container). `None` if unreadable.
fn mnt_ns_ino(pid: i64) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{pid}/ns/mnt")).ok().map(|m| m.ino())
}

/// A uid-1000 pid in the same mount namespace as the clone's root-owned main `pid`, whose
/// proc-root actually leads to the clone's home. Scans /proc once. `None` while the clone has no
/// uid-1000 session yet (still booting).
fn home_pid(main_pid: i64) -> Option<i64> {
    let target = mnt_ns_ino(main_pid)?;
    let mut candidates: Vec<(i64, u32, u64)> = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i64>() else { continue };
        if let (Some(uid), Some(ino)) = (proc_uid(pid), mnt_ns_ino(pid)) {
            candidates.push((pid, uid, ino));
        }
    }
    // `is_dir` follows the whole chain, so a chrooted process (whose root leads to a dead pid's
    // `fdinfo`) reads as absent rather than as a home. See `pick_home_pid`.
    pick_home_pid(target, &candidates, |pid| clone_home(pid).is_dir())
}

/// Names present under `hosts/` that no longer belong to a maintained clone and should be
/// removed (stopped, deleted, unmanaged, or a leftover from a previous run). Pure so it's
/// unit-testable: `existing` is the directory listing, `desired` the ids we linked this
/// tick.
fn entries_to_remove(existing: &[String], desired: &HashSet<String>) -> Vec<String> {
    existing.iter().filter(|n| !desired.contains(*n)).cloned().collect()
}

/// Create or repoint `link` → `target`, best-effort. A link already pointing at `target`
/// is left untouched; a stale symlink or a leftover non-symlink entry (e.g. an empty
/// sshfs-era mountpoint dir) is replaced. Failures are logged, not fatal — next tick
/// retries.
fn ensure_symlink(link: &Path, target: &Path, id: &str) {
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if std::fs::read_link(link).map(|cur| cur == target).unwrap_or(false) {
                return; // already correct
            }
            let _ = std::fs::remove_file(link); // stale symlink → replace
        }
        Ok(_) => {
            let _ = std::fs::remove_dir(link); // leftover (empty) real dir
        }
        Err(_) => {} // nothing there → just create
    }
    match std::os::unix::fs::symlink(target, link) {
        Ok(()) => tracing::info!(target: "homes", "linked {id} → {}", target.display()),
        Err(e) => tracing::warn!(target: "homes", "linking {id} → {}: {e}", target.display()),
    }
}

/// Remove `hosts/` entries not in `desired`. Only sweeps our own symlinks and empty
/// safe-named dirs (the is_safe_id guard keeps us from touching anything unexpected).
fn prune_stale(root: &Path, desired: &HashSet<String>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    for name in entries_to_remove(&names, desired) {
        let p = root.join(&name);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                if std::fs::remove_file(&p).is_ok() {
                    tracing::info!(target: "homes", "removed stale clone-home link {name}");
                }
            }
            // sshfs-era leftover mountpoint dir — sweep it if empty + safe-named.
            Ok(m) if m.is_dir() && is_safe_id(&name) => {
                let _ = std::fs::remove_dir(&p);
            }
            _ => {}
        }
    }
}

/// Ids [`ensure_now`] has linked that the store does not hold yet.
///
/// A create job links its clone a few hundred milliseconds before it registers it, and
/// [`prune_stale`] deletes the link for any id the store cannot account for. A tick landing
/// inside that window would delete the link the create job had just made, and the clone would
/// wait a whole [`RECONCILE_INTERVAL`] for it after all, which is the bug `ensure_now` exists to
/// remove. Ids are protected for exactly one pass, which is all that window ever needs, so a
/// create that dies before registering still gets its link swept on the pass after.
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

/// Protect one id from the next pass's prune.
fn protect(id: &str) {
    PENDING.lock().unwrap().insert(id.to_string());
}

/// Take the protected ids, clearing them.
fn take_protected() -> HashSet<String> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

/// What maintaining one clone's link produced. The caller decides from it whether to prune the
/// link, warn about a missing `pid: "host"`, or wait.
enum Outcome {
    /// The link now points at a live uid-1000 proc-root.
    Linked,
    /// No container pid: the clone is stopped or gone, so its link is stale.
    Gone,
    /// The clone's pid is not visible in our `/proc` (the operator left out `pid: "host"`).
    ProcInvisible,
    /// A daemon error, or a clone with no uid-1000 session yet. Keep any existing link.
    Waiting,
}

/// Point `hosts/<id>` at one clone's home. Best-effort: every failure is a retry next tick.
async fn ensure_for(app: &App, root: &Path, id: &str) -> Outcome {
    let pid = match app.docker.container_pid(id).await {
        Ok(Some(p)) => p,
        Ok(None) => return Outcome::Gone,
        // Daemon down / dev mode → quiet, retry next tick.
        Err(e) => {
            tracing::debug!(target: "homes", "pid probe for {id} failed: {e:#}");
            return Outcome::Waiting;
        }
    };
    if !proc_dir(pid).exists() {
        return Outcome::ProcInvisible;
    }
    // Link a uid-1000 process's proc-root (not the root-owned main pid), so the SMB share
    // (smbd → force user=rmng) can follow it.
    let Some(home) = home_pid(pid) else {
        return Outcome::Waiting;
    };
    ensure_symlink(&root.join(id), &clone_home(home), id);
    Outcome::Linked
}

/// Link one clone's home right now, waiting up to [`SESSION_WAIT`] for its uid-1000 session.
///
/// The create job calls this before it reports the clone ready. Home browsing, the SMB `clones`
/// share, token accounting, the transcript ledger and activity detection all read through this
/// link, so without it a clone is connectable but unreadable for up to [`RECONCILE_INTERVAL`].
///
/// The wait is what makes it reliable rather than merely likely: the link needs a process
/// running as the clone user, and on a clone whose daemon has only just registered there may be
/// none for another second or two. The loop is still the backstop, so a clone that never opens
/// a session costs [`SESSION_WAIT`] here and links later.
pub async fn ensure_now(app: &App, id: &str) {
    if !is_safe_id(id) {
        return;
    }
    let root = hosts_root(&app.config().data_dir);
    let _ = std::fs::create_dir_all(&root);
    protect(id);
    let deadline = Instant::now() + SESSION_WAIT;
    loop {
        match ensure_for(app, &root, id).await {
            Outcome::Waiting if Instant::now() < deadline => {
                tokio::time::sleep(SESSION_POLL).await
            }
            _ => return,
        }
    }
}

/// One reconcile pass. `warned` tracks clone ids we've already logged a missing-`/proc`
/// warning for, so the "add `pid: host`" hint fires once, not every tick.
async fn reconcile(app: &App, warned: &mut HashSet<String>) {
    let cfg = app.config();
    let root = hosts_root(&cfg.data_dir);
    let _ = std::fs::create_dir_all(&root);

    // Only managed clones (container name == clone id) with a path-safe id are candidates.
    let hosts: Vec<RmngClone> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived && is_safe_id(&h.id))
        .collect();

    // Ids we maintain a link for this tick; everything else under hosts/ gets pruned.
    let mut desired: HashSet<String> = HashSet::new();

    for h in &hosts {
        match ensure_for(app, &root, &h.id).await {
            Outcome::Linked => {
                warned.remove(&h.id); // resolved → allow a fresh warning if it ever recurs
                desired.insert(h.id.clone());
            }
            // Stopped / gone → no link (prune removes any stale one).
            Outcome::Gone => {
                warned.remove(&h.id);
            }
            Outcome::ProcInvisible => {
                if warned.insert(h.id.clone()) {
                    tracing::warn!(
                        target: "homes",
                        "clone {} pid not visible in /proc — add `pid: \"host\"` to the \
                         control-server service (compose.yaml) to browse clone homes under data/hosts",
                        h.id
                    );
                }
            }
            // Keep any existing link, so a transient blip doesn't thrash it.
            Outcome::Waiting => {
                warned.remove(&h.id);
                if root.join(&h.id).exists() {
                    desired.insert(h.id.clone());
                }
            }
        }
    }

    // A clone a create job has linked but not registered yet. Unioned here, with no await
    // between it and the prune below. See [`PENDING`].
    desired.extend(take_protected());
    prune_stale(&root, &desired);

    // Keep the once-warned set bounded to clones that still exist + are managed.
    let managed: HashSet<String> = hosts.iter().map(|h| h.id.clone()).collect();
    warned.retain(|id| managed.contains(id));
}

/// Background reconcile loop; spawned once at startup (matches `monitor::run`).
pub async fn run(app: App) {
    tracing::info!("clone-home reconciler started (data/hosts, every {}s)", RECONCILE_INTERVAL.as_secs());
    let mut warned: HashSet<String> = HashSet::new();
    loop {
        reconcile(&app, &mut warned).await;
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_linked_before_it_was_registered_survives_one_prune() {
        // The create job's own window: `ensure_now` has linked the clone, the store does not
        // hold it yet, and a pass runs in between.
        protect("fresh-clone");
        let mut desired: HashSet<String> = HashSet::new();
        desired.extend(take_protected());
        assert!(desired.contains("fresh-clone"));
        assert_eq!(
            entries_to_remove(&["fresh-clone".to_string()], &desired),
            Vec::<String>::new()
        );
        // One pass only, so a create that died before registering gets swept on the next.
        assert!(take_protected().is_empty());
    }

    #[test]
    fn hosts_root_joins_hosts() {
        assert_eq!(hosts_root("data"), Path::new("data/hosts"));
        assert_eq!(hosts_root("/srv/rmng/data"), Path::new("/srv/rmng/data/hosts"));
    }

    #[test]
    fn clone_home_targets_proc_root_home() {
        // With pid:host, /proc/<pid>/root is the clone's fs; home is the rmng user's.
        assert_eq!(clone_home(4321), PathBuf::from("/proc/4321/root/home/rmng"));
    }

    #[test]
    fn proc_dir_shape() {
        assert_eq!(proc_dir(17), PathBuf::from("/proc/17"));
    }

    #[test]
    fn pick_home_pid_wants_uid1000_in_target_ns() {
        let target = 42u64; // the clone's mount-namespace inode
        let cands = [(1i64, 0u32, 42u64), (37, 1000, 42), (99, 1000, 7)];
        assert_eq!(pick_home_pid(target, &cands, |_| true), Some(37));
        // Clone still booting — no uid-1000 process in its ns yet → None.
        assert_eq!(pick_home_pid(target, &[(1i64, 0u32, 42u64)], |_| true), None);
    }

    #[test]
    fn pick_home_pid_skips_a_sandboxed_browser_process() {
        // A Chrome/Firefox renderer is uid 1000 and in the clone's mount namespace, but it has
        // chrooted itself into /proc/<pid>/fdinfo, so its proc-root is not the clone's fs. Taking
        // it broke home browsing, token counting, and activity detection for as long as the tab
        // lived, because the pid never changed and the link was therefore never repointed.
        let target = 42u64;
        let cands = [(1i64, 0u32, 42u64), (868294, 1000, 42), (943649, 1000, 42)];
        let usable = |pid: i64| pid != 868294; // the renderer's root leads nowhere
        assert_eq!(pick_home_pid(target, &cands, usable), Some(943649));
    }

    #[test]
    fn pick_home_pid_takes_the_lowest_pid_that_works() {
        // Lowest first: within one clone that is the earliest-started process, which outlives
        // whatever a single turn spawned.
        let target = 42u64;
        let cands = [(900i64, 1000u32, 42u64), (100, 1000, 42), (500, 1000, 42)];
        assert_eq!(pick_home_pid(target, &cands, |_| true), Some(100));
    }

    #[test]
    fn pick_home_pid_is_none_when_no_candidate_resolves() {
        // Every uid-1000 process is sandboxed: no link is better than a broken one, and the
        // reconciler's prune then clears any link left over from before.
        let target = 42u64;
        let cands = [(868294i64, 1000u32, 42u64), (943649, 1000, 42)];
        assert_eq!(pick_home_pid(target, &cands, |_| false), None);
    }

    #[test]
    fn entries_to_remove_keeps_desired_drops_the_rest() {
        let existing = vec!["a".to_string(), "b".to_string(), "gone".to_string()];
        let desired: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        assert_eq!(entries_to_remove(&existing, &desired), vec!["gone".to_string()]);
        // No managed clones (empty desired) → everything on disk is stale.
        assert_eq!(entries_to_remove(&existing, &HashSet::new()), existing);
        // Nothing on disk → nothing to remove.
        assert!(entries_to_remove(&[], &desired).is_empty());
    }

    #[test]
    fn host_path_maps_a_clone_path_onto_its_browse_link() {
        let p = host_path("/data", "c1", "/home/rmng/proj").unwrap();
        assert_eq!(p, Path::new("/data/hosts/c1/proj"));
        // The home itself is addressable.
        assert_eq!(
            host_path("/data", "c1", "/home/rmng").unwrap(),
            Path::new("/data/hosts/c1")
        );
    }

    #[test]
    fn is_home_root_spots_the_home_and_nothing_under_it() {
        assert!(is_home_root("/home/rmng"));
        assert!(is_home_root("/home/rmng/"));
        assert!(!is_home_root("/home/rmng/proj"));
        assert!(!is_home_root("/home/rmng/.ssh"));
    }

    #[test]
    fn host_path_refuses_to_leave_the_clone_home() {
        // Above the home entirely.
        assert!(host_path("/data", "c1", "/etc/passwd").is_none());
        // Walking back out of it, which would otherwise reach the server's own files.
        assert!(host_path("/data", "c1", "/home/rmng/../../etc").is_none());
        assert!(host_path("/data", "c1", "/home/rmng/a/../../b").is_none());
        // A clone id that is really a path.
        assert!(host_path("/data", "../evil", "/home/rmng/proj").is_none());
    }
}
