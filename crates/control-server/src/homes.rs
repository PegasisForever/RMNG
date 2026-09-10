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
//! Links are static, not reconciled: the create job links its clone eagerly, the delete
//! job unlinks it, and a one-shot boot sync repairs crash windows. A missing dataset
//! simply means no link yet. Archived clones stay linked — their dataset is kept, so
//! stopped-clone browsing, ledgers, and token accounting keep working while archived.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use wire::RmngClone;

use crate::app::App;
use crate::files::is_safe_id;

/// The directory holding one symlink per managed clone home (`<data_dir>/hosts`).
/// `pub(crate)` so smb.rs single-sources the SMB share `path` from it (the share root is
/// exactly where the reconciler writes links, so the two can never diverge).
pub(crate) fn hosts_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("hosts")
}

/// The clone's home on the CT: its overlay merged view. Plain directory once mounted,
/// present whether the clone runs or not — this is what makes stopped-clone browsing work.
fn clone_home(id: &str) -> PathBuf {
    crate::home_overlay::merged_dir(crate::zfs::HOMES_DIR, id)
}

/// Names present under `hosts/` that no longer belong to a maintained clone and should be
/// removed (stopped, deleted, unmanaged, or a leftover from a previous run). Pure so it's
/// unit-testable: `existing` is the directory listing, `desired` the ids we linked this
/// tick.
fn entries_to_remove(existing: &[String], desired: &HashSet<String>) -> Vec<String> {
    existing
        .iter()
        .filter(|n| !desired.contains(*n))
        .cloned()
        .collect()
}

/// Create or repoint `link` → `target`, best-effort. A link already pointing at `target`
/// is left untouched; a stale symlink or a leftover non-symlink entry (e.g. an empty
/// sshfs-era mountpoint dir) is replaced. Failures are logged, not fatal — next tick
/// retries.
fn ensure_symlink(link: &Path, target: &Path, id: &str) {
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if std::fs::read_link(link)
                .map(|cur| cur == target)
                .unwrap_or(false)
            {
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
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
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

/// Point `hosts/<id>` at one clone's dataset dir. True when linked: the dataset exists
/// and the symlink is in place. False (deleted/pre-migration clone, missing dataset):
/// the caller prunes any stale entry.
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
/// create flow makes it first), and the boot sync is the backstop.
pub async fn ensure_now(app: &App, id: &str) {
    if !is_safe_id(id) {
        return;
    }
    let root = hosts_root(&app.data_dir());
    let _ = std::fs::create_dir_all(&root);
    ensure_for(app, &root, id).await;
}

/// Drop one clone's home link. The delete job calls this as its row goes away; the
/// dataset itself lives or dies by the ZFS destroy beside it.

pub async fn remove_link(app: &App, id: &str) {
    if !is_safe_id(id) {
        return;
    }
    let p = hosts_root(&app.data_dir()).join(id);
    if matches!(
        std::fs::symlink_metadata(&p),
        Ok(m) if m.file_type().is_symlink()
    ) {
        let _ = std::fs::remove_file(&p);
    }
}

/// One-shot sync: link every managed clone with a dataset dir (archived included —
/// their dataset is kept, so stopped homes stay browsable), prune the rest. Runs once
/// at boot to repair crash windows; the create job links eagerly and the delete job
/// unlinks, so nothing ticks.
pub async fn sync_all(app: App) {
    let root = hosts_root(&app.data_dir());
    let _ = std::fs::create_dir_all(&root);

    // Only managed clones with a path-safe id are candidates — archived included.
    let hosts: Vec<RmngClone> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && is_safe_id(&h.id))
        .collect();

    // Ids we maintain a link for this tick; everything else under hosts/ gets pruned.
    let mut desired: HashSet<String> = HashSet::new();

    for h in &hosts {
        // Linked → keep; missing dataset (deleted, pre-migration) → prune the stale entry.
        if ensure_for(&app, &root, &h.id).await {
            desired.insert(h.id.clone());
        }
    }

    prune_stale(&root, &desired);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_root_joins_hosts() {
        assert_eq!(hosts_root("data"), Path::new("data/hosts"));
        assert_eq!(
            hosts_root("/srv/rmng/data"),
            Path::new("/srv/rmng/data/hosts")
        );
    }

    #[test]
    fn clone_home_targets_the_merged_view() {
        // Gen-2: the browse link points at the overlay merged view, running or not.
        assert_eq!(
            clone_home("c1"),
            PathBuf::from("/srv/rmng-homes/.merged/c1")
        );
    }

    #[test]
    fn entries_to_remove_keeps_desired_drops_the_rest() {
        let existing = vec!["a".to_string(), "b".to_string(), "gone".to_string()];
        let desired: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        assert_eq!(
            entries_to_remove(&existing, &desired),
            vec!["gone".to_string()]
        );
        // No managed clones (empty desired) → everything on disk is stale.
        assert_eq!(entries_to_remove(&existing, &HashSet::new()), existing);
        // Nothing on disk → nothing to remove.
        assert!(entries_to_remove(&[], &desired).is_empty());
    }
}
