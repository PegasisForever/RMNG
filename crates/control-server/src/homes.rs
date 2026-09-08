//! `data/hosts/<id>` — every managed clone's home (`/home/rmng`) surfaced in one place, so
//! all clones' files are browsable from a single directory (on the control-server box, and
//! on the Docker host itself via the `rmng-data` volume at
//! `/var/lib/docker/volumes/rmng-data/_data/hosts/…`).
//!
//! Gen-2: each clone's home is its own ZFS dataset, bind-mounted at `/home/rmng` and
//! visible on the CT at `/srv/rmng-homes/<id>`, so the browse entry is a plain symlink
//! `<data_dir>/hosts/<id>` → `/srv/rmng-homes/<id>`. It exists running or stopped — no
//! PID chasing, no `pid: "host"` requirement. The retired `/proc/<pid>/root` reader is
//! gone (see stage 4 deletions for the rest of gen-1).
//!
//! A 15s reconcile loop links clones with a dataset dir and removes entries for
//! deleted/unmanaged clones. Best-effort throughout: a missing dataset just retries next
//! tick.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use wire::RmngClone;

use crate::app::App;
use crate::files::is_safe_id;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

/// The directory holding one symlink per managed clone home (`<data_dir>/hosts`).
/// `pub(crate)` so smb.rs single-sources the SMB share `path` from it (the share root is
/// exactly where the reconciler writes links, so the two can never diverge).
pub(crate) fn hosts_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("hosts")
}

/// The clone's home on the CT: its ZFS dataset dir. Plain directory, present whether the
/// clone runs or not — this is what makes stopped-clone browsing work.
fn clone_home(id: &str) -> PathBuf {
    PathBuf::from(crate::zfs::dataset_dir(id))
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

/// Point `hosts/<id>` at one clone's dataset dir. True when linked: the dataset exists
/// and the symlink is in place. False (stopped/deleted/pre-migration clone, missing
/// dataset): the caller prunes any stale entry. Best-effort — next tick retries.
async fn ensure_for(_app: &App, root: &Path, id: &str) -> bool {
    let target = clone_home(id);
    if !target.is_dir() {
        return false;
    }
    ensure_symlink(&root.join(id), &target, id);
    true
}

/// Link one clone's home right now.
///
/// The create job calls this before it reports the clone ready. Home browsing, the SMB
/// `clones` share, token accounting, the transcript ledger and activity detection all read
/// through this link. No wait: the dataset exists by the time the container does (the
/// create flow makes it first), and the reconcile loop is the backstop.
pub async fn ensure_now(app: &App, id: &str) {
    if !is_safe_id(id) {
        return;
    }
    let root = hosts_root(&app.config().data_dir);
    let _ = std::fs::create_dir_all(&root);
    protect(id);
    ensure_for(app, &root, id).await;
}

/// One reconcile pass: link every managed clone with a dataset dir, prune the rest.
async fn reconcile(app: &App) {
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
        // Linked → keep; missing dataset (deleted, pre-migration) → prune the stale entry.
        if ensure_for(app, &root, &h.id).await {
            desired.insert(h.id.clone());
        }
    }

    // A clone a create job has linked but not registered yet. Unioned here, with no await
    // between it and the prune below. See [`PENDING`].
    desired.extend(take_protected());
    prune_stale(&root, &desired);
}

/// Background reconcile loop; spawned once at startup (matches `monitor::run`).
pub async fn run(app: App) {
    tracing::info!("clone-home reconciler started (data/hosts, every {}s)", RECONCILE_INTERVAL.as_secs());
    loop {
        reconcile(&app).await;
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
    fn clone_home_targets_the_dataset_dir() {
        // Gen-2: the browse link points at the CT-side dataset dir, running or not.
        assert_eq!(clone_home("c1"), PathBuf::from("/srv/rmng-homes/c1"));
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

}
