//! `<data_dir>/shared` mounted into every running clone at `/home/rmng/shared`, and served as
//! the `shared` SMB share beside `clones` (see [`crate::smb`]). One flat pool: every clone sees
//! the same bytes and so does an SMB client, so a file dropped anywhere appears everywhere.
//! Read-write from both sides, which is what the root directory's uid-1000 owner buys.
//!
//! ## Why a reconciler rather than a container mount
//!
//! Docker cannot add a mount to an existing container, and recreating one destroys the clone's
//! writable layer. Every clone on a live fleet predates this feature, so a create-time mount
//! would reach none of them. A live in-place mount reaches all of them, which is the same call
//! [`crate::shm`] makes for `/dev/shm`.
//!
//! ## Why the new mount API
//!
//! Two ordinary approaches both fail with `EINVAL`, measured against a running clone:
//!
//! * `mount --bind <src> /proc/<pid>/root/home/rmng/shared` from the server. The mount would
//!   land in the server's own mount table, and the kernel refuses the cross-namespace target.
//! * `nsenter -t <pid> -m -- mount --bind <src> …`. After the `setns` the source path no longer
//!   resolves, and handing it over as a pre-opened `/proc/self/fd/N` is refused too.
//!
//! `open_tree(OPEN_TREE_CLONE)` detaches a copy of the source subtree while we can still see it,
//! and `move_mount` attaches that copy after the `setns`. That pair is the only route across a
//! mount-namespace boundary. Both syscalls need Linux 5.2 or newer.
//!
//! ## Why a throwaway thread
//!
//! `setns(CLONE_NEWNS)` returns `EINVAL` whenever the caller's `fs_struct` is shared, and every
//! thread in a tokio process shares one. `unshare(CLONE_FS)` gives the calling thread a private
//! `fs_struct` and makes the `setns` succeed. That is also why [`mount_into`] must own a thread
//! and end it: the thread stays in the clone's mount namespace for the rest of its life, so a
//! tokio worker or a pooled `spawn_blocking` thread would resolve every later path it touched
//! inside that one clone.
//!
//! A live mount dies with the container, because Docker rebuilds the clone's mount table on
//! every start. The 15s loop re-applies it within one tick, so the mount survives a restart the
//! way the `/dev/shm` resize does. Harmless without `pid: "host"`: the reconciler warns once per
//! clone and mounts nothing.

use std::collections::HashSet;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wire::RmngClone;

use crate::app::App;
use crate::docker::CLONE_USER;
use crate::files::is_safe_id;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

/// The clone user's uid and gid (see [`crate::docker::CLONE_USER`]). The pool's root directory
/// carries this owner so a clone writing through the mount and smbd writing through the
/// `shared` share both land as the same user.
const CLONE_UID: u32 = 1000;

/// Flags for the two mount syscalls nix 0.29 does not wrap.
const OPEN_TREE_CLONE: libc::c_uint = 1;
const AT_RECURSIVE: libc::c_uint = 0x8000;
const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x0000_0004;

/// The directory holding the shared pool (`<data_dir>/shared`). `pub(crate)` so smb.rs
/// single-sources the `shared` share path from it, as it already does for `hosts`.
pub(crate) fn shared_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("shared")
}

/// Where the pool appears inside every clone.
fn clone_target() -> String {
    format!("/home/{CLONE_USER}/shared")
}

/// The outcome of reconciling one clone. The loop uses it to warn once about a missing
/// `pid: "host"` instead of every tick.
enum Outcome {
    /// Mounted this tick.
    Mounted,
    /// Already mounted, not running, or a transient failure that the next tick retries.
    Skipped,
    /// The clone's PID is not visible in our `/proc` (the operator left out `pid: "host"`).
    ProcInvisible,
}

/// Whether `target` is already a mount point, given a process's `/proc/<pid>/mountinfo`.
///
/// A mountinfo line reads `<fields…> - <fstype> <source> <super-options>`, and the mount point
/// is the 5th pre-separator field. Pure so it is unit-testable.
fn is_mounted(mountinfo: &str, target: &str) -> bool {
    mountinfo.lines().any(|line| {
        line.split_once(" - ")
            .and_then(|(fields, _)| fields.split_whitespace().nth(4))
            == Some(target)
    })
}

/// Detach a copy of `src` from our own mount namespace, enter the mount namespace of host PID
/// `pid`, and attach the copy at `target`.
///
/// Leaves the calling thread inside the clone's namespace, so [`mount_shared`] is the only
/// caller and it passes a thread it is about to drop.
fn mount_into(pid: i64, src: &Path, target: &str) -> io::Result<()> {
    let src_c = CString::new(src.as_os_str().as_bytes())?;
    let target_c = CString::new(target)?;
    let empty = CString::new("")?;

    // Open the namespace handle first. After the `setns` below this path resolves inside the
    // clone, where the same number names a different process.
    let ns = std::fs::File::open(format!("/proc/{pid}/ns/mnt"))?;

    // `open_tree` while we can still see `src`. The copy is detached, so it is visible nowhere
    // until `move_mount` attaches it.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            libc::AT_FDCWD,
            src_c.as_ptr(),
            OPEN_TREE_CLONE | AT_RECURSIVE,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let tree = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };

    // A shared `fs_struct` makes `setns(CLONE_NEWNS)` fail with EINVAL. Unsharing it is what
    // makes this thread single-use.
    if unsafe { libc::unshare(libc::CLONE_FS) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::setns(ns.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
        return Err(io::Error::last_os_error());
    }

    // `target` now resolves inside the clone, where `ensure_for` already created it.
    let moved = unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            tree.as_raw_fd(),
            empty.as_ptr(),
            libc::AT_FDCWD,
            target_c.as_ptr(),
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    };
    if moved < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Run [`mount_into`] on a thread of its own and wait for it.
///
/// The thread ends up in the clone's mount namespace, so it must never be a tokio worker or a
/// pooled `spawn_blocking` thread. Five syscalls of work, so the join costs about what spawning
/// the thread costs.
fn mount_shared(pid: i64, src: PathBuf, target: String) -> io::Result<()> {
    std::thread::spawn(move || mount_into(pid, &src, &target))
        .join()
        .map_err(|_| io::Error::other("shared-mount thread panicked"))?
}

/// Mount the pool into one clone. Best-effort: a transient failure returns `Skipped` and the
/// next tick retries. Logs once per mount, and the idempotency check keeps every other tick
/// silent.
async fn ensure_for(app: &App, id: &str, src: &Path) -> Outcome {
    let pid = match app.docker.container_pid(id).await {
        Ok(Some(p)) => p,
        Ok(None) => return Outcome::Skipped, // stopped or gone
        Err(e) => {
            tracing::debug!(target: "shared", "pid probe for {id} failed: {e:#}");
            return Outcome::Skipped;
        }
    };

    // Read the clone's mount table from OUR view, which `pid: "host"` makes possible without an
    // nsenter. It doubles as the pid-visibility probe.
    let mountinfo = match std::fs::read_to_string(format!("/proc/{pid}/mountinfo")) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Outcome::ProcInvisible,
        Err(e) => {
            tracing::debug!(target: "shared", "reading /proc/{pid}/mountinfo for {id}: {e}");
            return Outcome::Skipped;
        }
    };

    let target = clone_target();
    if is_mounted(&mountinfo, &target) {
        return Outcome::Skipped;
    }

    // `move_mount` needs its mount point to exist. Create it through the clone's proc-root and
    // give it the clone user, so the bare directory still reads right in the window between a
    // container start and the next tick.
    let in_clone = PathBuf::from(format!("/proc/{pid}/root{target}"));
    if let Err(e) = std::fs::create_dir_all(&in_clone) {
        tracing::warn!(target: "shared", "creating {target} in {id}: {e}");
        return Outcome::Skipped;
    }
    let _ = std::os::unix::fs::chown(&in_clone, Some(CLONE_UID), Some(CLONE_UID));

    let (src, tgt) = (src.to_path_buf(), target.clone());
    match tokio::task::spawn_blocking(move || mount_shared(pid, src, tgt)).await {
        Ok(Ok(())) => tracing::info!(target: "shared", "mounted {target} in {id} (pid {pid})"),
        Ok(Err(e)) => {
            tracing::warn!(target: "shared", "mounting {target} in {id} (pid {pid}): {e}");
            return Outcome::Skipped;
        }
        Err(e) => {
            tracing::warn!(target: "shared", "shared-mount task for {id} failed: {e}");
            return Outcome::Skipped;
        }
    }
    Outcome::Mounted
}

/// One reconcile pass over every running managed clone. `warned` tracks ids we already logged a
/// missing-`pid: "host"` warning for, so the hint fires once rather than every tick.
async fn reconcile(app: &App, warned: &mut HashSet<String>) {
    let cfg = app.config();
    let src = shared_root(&cfg.data_dir);

    let hosts: Vec<RmngClone> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived && is_safe_id(&h.id))
        .collect();

    for h in &hosts {
        match ensure_for(app, &h.id, &src).await {
            Outcome::ProcInvisible => {
                if warned.insert(h.id.clone()) {
                    tracing::warn!(
                        target: "shared",
                        "clone {} pid not visible in /proc. Add `pid: \"host\"` to the \
                         control-server service (compose.yaml) to mount the shared folder",
                        h.id
                    );
                }
            }
            _ => {
                warned.remove(&h.id); // resolved, so allow a fresh warning if it recurs
            }
        }
    }

    // Keep the once-warned set bounded to clones that still exist and are managed.
    let managed: HashSet<String> = hosts.iter().map(|h| h.id.clone()).collect();
    warned.retain(|id| managed.contains(id));
}

/// Create the pool, then reconcile it into every clone forever. Spawned once at startup
/// alongside [`crate::homes::run`].
pub async fn run(app: App) {
    let root = shared_root(&app.config().data_dir);
    if let Err(e) = std::fs::create_dir_all(&root) {
        tracing::error!(target: "shared", "creating {}: {e}", root.display());
        return;
    }
    // Owned by the clone user, so a clone writing through the mount needs nothing further.
    if let Err(e) = std::os::unix::fs::chown(&root, Some(CLONE_UID), Some(CLONE_UID)) {
        tracing::warn!(target: "shared", "chown {}: {e}", root.display());
    }
    tracing::info!(
        "shared-folder reconciler started ({} → {}, every {}s)",
        root.display(),
        clone_target(),
        RECONCILE_INTERVAL.as_secs()
    );

    let mut warned: HashSet<String> = HashSet::new();
    loop {
        reconcile(&app, &mut warned).await;
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTED: &str = "\
843 771 0:58 / /etc/hosts rw,relatime shared:412 - ext4 /dev/sda1 rw
901 771 0:99 / /home/rmng/shared rw,relatime - ext4 /dev/sda1 rw
846 771 0:61 / /proc rw,relatime - proc proc rw";

    #[test]
    fn shared_root_joins_shared() {
        assert_eq!(shared_root("data"), Path::new("data/shared"));
        assert_eq!(shared_root("/srv/rmng/data"), Path::new("/srv/rmng/data/shared"));
    }

    #[test]
    fn clone_target_is_under_the_clone_users_home() {
        assert_eq!(clone_target(), "/home/rmng/shared");
    }

    #[test]
    fn is_mounted_finds_the_target() {
        assert!(is_mounted(MOUNTED, "/home/rmng/shared"));
    }

    #[test]
    fn is_mounted_false_when_absent() {
        let without = "846 771 0:61 / /proc rw,relatime - proc proc rw";
        assert!(!is_mounted(without, "/home/rmng/shared"));
    }

    #[test]
    fn is_mounted_ignores_a_similarly_named_mount_point() {
        // A prefix match would report the pool as present and skip the clone forever.
        let other = "901 771 0:99 / /home/rmng/shared-old rw - ext4 /dev/sda1 rw";
        assert!(!is_mounted(other, "/home/rmng/shared"));
    }

    #[test]
    fn is_mounted_ignores_a_path_that_only_appears_as_a_mount_source() {
        // The source sits after the ` - ` separator, so it must not be read as a mount point.
        let source_only = "901 771 0:99 / /mnt/x rw - ext4 /home/rmng/shared rw";
        assert!(!is_mounted(source_only, "/home/rmng/shared"));
    }
}
