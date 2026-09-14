//! One clone's home, as one value that owns every path it has and its whole lifecycle.
//!
//! A clone's home is a single thing seen five ways: the ZFS dataset NAME
//! (`<parent>/<id>`), the CT-side directory that dataset mounts at
//! (`/srv/rmng-homes/<id>`), the overlay upper and work dirs inside that directory, and
//! the merged view (`/srv/rmng-homes/.merged/<id>`) that binds at `/home/rmng`. Each
//! spelling used to be re-derived at the call site, from whichever of the five the
//! caller happened to hold, and a name is not a directory: passing the dataset NAME
//! where the overlay wanted the DIRECTORY gave overlayfs a relative `upperdir` that
//! resolved to nothing, and every clone came up on the bare template home. A create
//! path that recorded the bare id where a dataset name belonged cost a repair pass on
//! top of that.
//!
//! `CloneHome` is the interface: a caller holds one and asks it questions.
//! [`crate::zfs`] and [`crate::home_overlay`] stay the implementation — the `zfs`
//! invocations, the overlay mount, the skeleton export — and nothing outside this module
//! derives a home path any more. That is the leverage: a change to the layout is a
//! change to this one module, and a caller cannot pick the wrong spelling because it
//! never sees more than one.
//!
//! The value is two strings and no state, so build one from the id wherever you need it
//! and drop it. Nothing is cached: a changed `docker.homes_parent` applies to the next
//! call, which is how the rest of the server treats config.

use std::path::PathBuf;

use anyhow::{Context, Result};
use wire::RmngClone;

use crate::app::App;

/// Whether a state row is a gen-2 clone (its home is a ZFS dataset this server owns).
///
/// The marker is the PRESENCE of `RmngClone::dataset`, never its value. The value is
/// always `<parent>/<id>` and nothing reads it any more — every path comes from
/// [`CloneHome`] instead — but the field is still written and still persisted in
/// `state.json`, because `Some`/`None` is the only record of which generation a clone
/// belongs to. Named here so the call sites that branch on it say what they mean
/// instead of testing a path string for emptiness.
pub(crate) fn is_gen2(h: &RmngClone) -> bool {
    h.dataset.is_some()
}

/// The directory holding every clone's merged home, one entry per clone with a live
/// home. Bound into every clone at `/clones` (see
/// [`crate::docker::CreateSpec::browse_root`]) and the target of every `<data_dir>/hosts`
/// link the SMB share serves, so all three browse paths show one directory.
///
/// Fleet-wide, not per-clone, so it is a free function rather than a [`CloneHome`]
/// method.
pub(crate) fn browse_root() -> PathBuf {
    crate::home_overlay::merged_root(crate::zfs::HOMES_DIR)
}

/// One clone's home: its dataset, its overlay, and the operations that create, mount,
/// unmount and destroy them.
pub(crate) struct CloneHome {
    id: String,
    parent: String,
}

impl CloneHome {
    /// The home of clone `id` on this server. Reads `docker.homes_parent` fresh (the
    /// pool name differs per host, and config is immediate-apply).
    pub(crate) fn of(app: &App, id: &str) -> Self {
        Self::new(&app.config().docker.homes_parent, id)
    }

    /// The home of clone `id` under an explicit parent — tests, and any caller that
    /// already holds the parent.
    pub(crate) fn new(parent: &str, id: &str) -> Self {
        Self {
            id: id.to_string(),
            parent: parent.to_string(),
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// The ZFS dataset NAME, `<parent>/<id>`. Both home sources land on it — `zfs create`
    /// for a fresh clone, `zfs clone` for a fork — so it is fully derivable and no caller
    /// has to remember it.
    ///
    /// This is what the create path stores in `RmngClone::dataset`. It
    /// stores it for its presence alone (see [`is_gen2`]); no reader takes the value back
    /// out.
    pub(crate) fn dataset(&self) -> String {
        crate::zfs::dataset_name(&self.parent, &self.id)
    }

    /// The CT-side DIRECTORY the dataset mounts at. Every mount path is built from this,
    /// never from [`Self::dataset`]: the daemon rejects a relative bind source, and a
    /// dataset name read as a path is relative.
    pub(crate) fn dataset_dir(&self) -> PathBuf {
        PathBuf::from(crate::zfs::dataset_dir(&self.id))
    }

    /// The clone's read-write delta (overlay upper), inside the dataset.
    pub(crate) fn upper(&self) -> PathBuf {
        crate::home_overlay::upper_dir(&self.dataset_dir())
    }

    /// Overlay workdir: same filesystem as the upper, outside it.
    pub(crate) fn work(&self) -> PathBuf {
        crate::home_overlay::work_dir(&self.dataset_dir())
    }

    /// The merged view, bound at `/home/rmng`. This is the clone's home as anything
    /// outside the server sees it: the browse link, the SMB share, and every direct
    /// file read or write into a live home.
    pub(crate) fn merged(&self) -> PathBuf {
        crate::home_overlay::merged_dir(crate::zfs::HOMES_DIR, &self.id)
    }

    /// `zfs create` a fresh dataset for this clone.
    ///
    /// Deliberately not an `ensure_`: the create path calls it exactly once,
    /// on an id that has no dataset, and an id that already has one is a bug worth an
    /// error rather than a silent reuse. [`crate::provision::HomeSource::Reuse`] is how a
    /// caller says the dataset is already there.
    pub(crate) fn create_dataset(&self) -> Result<()> {
        crate::zfs::create(&self.parent, &self.id)
    }

    /// `zfs clone` this clone's dataset out of `snapshot` (the fork path).
    pub(crate) fn clone_dataset_from(&self, snapshot: &str) -> Result<()> {
        crate::zfs::clone_dataset(&self.parent, snapshot, &self.id)?;
        Ok(())
    }

    /// `zfs snapshot` this clone's dataset. Returns the full `name@snap`.
    pub(crate) fn snapshot(&self, snap: &str) -> Result<String> {
        crate::zfs::snapshot(&self.parent, &self.id, snap)
    }

    /// Drop a snapshot under the same homes parent when nothing was cloned from it.
    /// Origin snapshots pair 1:1 with fork clones, so a plain destroy suffices; a busy
    /// snapshot surfaces as an error for the caller to keep.
    pub(crate) fn drop_snapshot(&self, snapshot: &str) -> Result<()> {
        crate::zfs::destroy_snapshot_if_unreferenced(&self.parent, snapshot)
    }

    /// The snapshot this home was cloned from, if any. `None` for a fresh dataset
    /// (origin `-`) and on any error — the delete path uses it to clean up a fork's
    /// origin snapshot, and "cannot tell" and "there is none" call for the same action.
    pub(crate) fn origin(&self) -> Option<String> {
        crate::zfs::origin(&self.parent, &self.id)
    }

    /// Ensure the dataset is mounted at its pinned mountpoint. Idempotent.
    ///
    /// A CT reboot leaves every per-clone dataset UNMOUNTED. There is deliberately no
    /// `zfsutils-linux` inside the CT (its `zfs-dkms` cannot build under a shared kernel),
    /// so nothing runs the usual `zfs mount -a` at boot — the server is the only thing
    /// that can. Skipping this let the boot remount stack an overlay on an empty
    /// directory and hand every clone a pristine template home while its real one sat
    /// unmounted, which is exactly what CT 204 did after its first reboot.
    pub(crate) fn ensure_mounted(&self) -> Result<()> {
        crate::zfs::ensure_mounted(&self.dataset())
    }

    /// Make sure the dataset holds the overlay upper + work dirs.
    ///
    /// Public, and not folded into [`Self::ensure_overlay`], because the retired one-shot
    /// migration wrote the old home straight into [`Self::upper`] long before there was
    /// an image to mount over it.
    pub(crate) fn ensure_layout(&self) -> Result<()> {
        for dir in [self.upper(), self.work()] {
            std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        }
        Ok(())
    }

    /// Bring the home up on `image_tag`: export that image's `/home/rmng` as the shared
    /// overlay lower, mount (or remount, when the lower changed) the overlay, and point
    /// `~/clones` and `~/shared` at the two mounts that live outside the home.
    ///
    /// Returns `true` when this call ESTABLISHED the mount, as opposed to finding it
    /// already correct. A container Docker started before that moment captured the bare
    /// mountpoint, and its bind is PRIVATE, so this mount will never propagate into it —
    /// on a CT reboot the clones routinely win that race (measured at 84 ms) and come up
    /// showing the template home with the real one nowhere in sight. The boot remount
    /// restarts exactly the clones this returns `true` for.
    pub(crate) async fn ensure_overlay(&self, app: &App, image_tag: &str) -> Result<bool> {
        self.ensure_layout()?;
        let digest = crate::home_overlay::ensure_skeleton(app, image_tag).await?;
        let merged = self.merged();
        debug_assert!(
            merged.is_absolute(),
            "overlay merged view must be an absolute bind source"
        );
        let established =
            crate::home_overlay::ensure_mounted(&self.dataset_dir(), &digest, &merged).await?;
        // Here rather than only on create, so a fleet that predates the move of `/clones`
        // and `/shared` out of the home is fixed by one boot instead of a recreate.
        crate::home_overlay::ensure_home_links(&merged, &self.id);
        Ok(established)
    }

    /// Tear the merged view down (delete path): unmount, then remove the mountpoint so
    /// no dangling directory survives. The dataset is untouched — [`Self::destroy`] is a
    /// separate decision, and the overlay has to come down first either way because it
    /// pins the dataset busy.
    pub(crate) fn teardown(&self) {
        crate::home_overlay::teardown_merged(&self.merged());
    }

    /// `zfs destroy [-r]` the dataset and drop its mountpoint directory.
    pub(crate) fn destroy(&self, recursive: bool) -> Result<()> {
        crate::zfs::destroy(&self.parent, &self.id, recursive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The five spellings, from one value, for one clone.
    #[test]
    fn every_path_comes_off_the_same_id() {
        let home = CloneHome::new("tank/rmng/homes", "pega-x");
        assert_eq!(home.id(), "pega-x");
        assert_eq!(home.dataset(), "tank/rmng/homes/pega-x");
        assert_eq!(home.dataset_dir(), Path::new("/srv/rmng-homes/pega-x"));
        assert_eq!(home.upper(), Path::new("/srv/rmng-homes/pega-x/upper"));
        assert_eq!(home.work(), Path::new("/srv/rmng-homes/pega-x/work"));
        assert_eq!(home.merged(), Path::new("/srv/rmng-homes/.merged/pega-x"));
    }

    /// The dataset NAME is not a path, and the mount paths never come from it. Reading
    /// it as one gives a relative `upperdir` that resolves to nothing.
    #[test]
    fn the_dataset_name_is_never_a_mount_path() {
        let home = CloneHome::new("rpool/rmng/homes", "c1");
        assert!(!Path::new(&home.dataset()).is_absolute());
        assert!(home.dataset_dir().is_absolute());
        assert!(home.merged().is_absolute());
    }

    /// A different pool changes the dataset name and nothing else: the mountpoint is
    /// pinned at create time and is the same on every host.
    #[test]
    fn the_parent_only_moves_the_dataset_name() {
        let a = CloneHome::new("tank/rmng/homes", "c1");
        let b = CloneHome::new("rpool/rmng/homes", "c1");
        assert_ne!(a.dataset(), b.dataset());
        assert_eq!(a.dataset_dir(), b.dataset_dir());
        assert_eq!(a.merged(), b.merged());
    }

    #[test]
    fn browse_root_is_the_merged_dir_every_home_sits_under() {
        let home = CloneHome::new("tank/rmng/homes", "c1");
        assert_eq!(browse_root().join("c1"), home.merged());
    }
}
