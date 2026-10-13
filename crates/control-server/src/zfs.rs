//! ZFS home-dataset wrapper for gen-2 clones.
//!
//! Each gen-2 clone owns one dataset under a configured parent (e.g.
//! `tank/rmng/homes/<id>` or `rpool/rmng/homes/<id>`), visible in the outer CT at
//! `<HOMES_DIR>/<id>`. Fork is `snapshot` + `clone`; delete is `destroy` plus
//! origin-snapshot cleanup.
//!
//! The parent pool name differs per host, so every function takes `parent` from
//! `docker.homes_parent` config (default `tank/rmng/homes`). Every path is validated
//! to stay under that parent: the outer CT runs privileged with `/dev/zfs`, so CT
//! root could destroy any pool dataset. All ZFS calls in the server go through this
//! module — no raw `zfs` invocation elsewhere.
//!
//! This is the implementation, not the interface: [`crate::clone_home::CloneHome`] owns
//! a clone's home and is what callers hold. Everything below it is `pub(crate)` and
//! called from there, so no caller has to know that a dataset name and a dataset
//! directory are two different strings.
//!
//! Mount visibility: a dataset created from inside a container mounts only in that
//! container's mount namespace, invisible to dockerd. So `create`/`clone` pin
//! `-o mountpoint=<HOMES_DIR>/<id>`, and the rmng container bind-mounts the homes
//! dir `rshared` — the mount then propagates into dockerd's namespace.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// CT-side dir where the homes parent dataset is mounted (one bind mount, host-owned).
pub const HOMES_DIR: &str = "/srv/rmng-homes";

/// Ensure `/dev/zfs` exists, creating the node when missing.
///
/// The outer CT must NOT bind-mount the host's `/dev/zfs` (a shared devtmpfs bind
/// breaks nested container mount joins: every `docker exec` silently lands on CT
/// files). Instead the CT carries `lxc.cgroup2.devices.allow: c 10:249 rwm` and this
/// node, created here with `mknod`. `/dev` is tmpfs, so the node vanishes on CT
/// reboot — this runs at every server boot to self-heal it.
///
/// Major:minor `10:249` is hardcoded: it is the standard ZFS misc-device number on
/// Linux (stable across ZFS releases; verified against the host with `ls -l /dev/zfs`).
/// Non-fatal: logs a loud warning and returns, so the server still boots for repair.
pub fn ensure_dev_zfs() {
    if Path::new("/dev/zfs").exists() {
        return;
    }
    tracing::warn!("/dev/zfs missing (tmpfs /dev lost it on CT reboot); recreating node c 10:249");
    let dev = nix::sys::stat::makedev(10, 249);
    if let Err(e) = nix::sys::stat::mknod(
        "/dev/zfs",
        nix::sys::stat::SFlag::S_IFCHR,
        nix::sys::stat::Mode::from_bits_truncate(0o666),
        dev,
    ) {
        tracing::warn!(
            "/dev/zfs mknod failed (need privileged CT + devices.allow c 10:249): {e:#}"
        );
        return;
    }
    match run(&["list", "-H", "-o", "name"]) {
        Ok(_) => tracing::info!("/dev/zfs recreated and `zfs list` works"),
        Err(e) => tracing::warn!("/dev/zfs node created but `zfs list` still fails: {e:#}"),
    }
}

/// CT-side path of a clone's home dataset dir.
/// Reached through [`crate::clone_home::CloneHome::dataset_dir`].
pub(crate) fn dataset_dir(clone_id: &str) -> String {
    format!("{HOMES_DIR}/{clone_id}")
}

/// Dataset name for a clone id under `parent`.
/// Reached through [`crate::clone_home::CloneHome::dataset`].
pub(crate) fn dataset_name(parent: &str, clone_id: &str) -> String {
    format!("{parent}/{clone_id}")
}

/// Reject anything that is not a direct child path of `parent`: no `@` or `/`
/// beyond the child name, no `..`, no absolute escapes.
fn check_dataset(parent: &str, name: &str) -> Result<()> {
    let rest = name
        .strip_prefix(&format!("{parent}/"))
        .ok_or_else(|| anyhow::anyhow!("zfs: {name:?} is outside {parent}"))?;
    if rest.is_empty()
        || rest.contains('/')
        || rest.contains('@')
        || rest.contains('\0')
        || Path::new(rest)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("zfs: {name:?} is not a direct child of {parent}");
    }
    Ok(())
}

fn check_snapshot(parent: &str, snap: &str) -> Result<()> {
    let (ds, name) = snap
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("zfs: snapshot {snap:?} needs name@snapshot"))?;
    check_dataset(parent, ds)?;
    if name.is_empty() || name.contains(['/', '@', '\0']) {
        anyhow::bail!("zfs: bad snapshot name in {snap:?}");
    }
    Ok(())
}

fn run(args: &[&str]) -> Result<String> {
    let out = Command::new("zfs")
        .args(args)
        .output()
        .context("spawning zfs")?;
    if !out.status.success() {
        anyhow::bail!(
            "zfs {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `zfs create -o mountpoint=<HOMES_DIR>/<clone-id> <parent>/<clone-id>`.
/// The pinned mountpoint keeps the dataset visible under the homes bind mount
/// (children would otherwise auto-mount at the pool path, e.g. `/rpool/...`).
pub(crate) fn create(parent: &str, clone_id: &str) -> Result<()> {
    let ds = dataset_name(parent, clone_id);
    check_dataset(parent, &ds)?;
    let mp = format!("mountpoint={}", dataset_dir(clone_id));
    run(&["create", "-o", &mp, &ds])?;
    Ok(())
}

/// Ensure `dataset` is mounted at its recorded mountpoint. Idempotent.
/// Why the server has to do this at all:
/// [`crate::clone_home::CloneHome::ensure_mounted`].
pub(crate) fn ensure_mounted(dataset: &str) -> Result<()> {
    if dataset.is_empty() || dataset.contains(['\0', ' ', '\n']) {
        anyhow::bail!("zfs: refusing to mount {dataset:?}");
    }
    if run(&["get", "-H", "-o", "value", "mounted", dataset])?.trim() == "yes" {
        return Ok(());
    }
    run(&["mount", dataset])?;
    Ok(())
}

/// `zfs snapshot <parent>/<clone-id>@<snap>`.
pub(crate) fn snapshot(parent: &str, clone_id: &str, snap: &str) -> Result<String> {
    let full = format!("{}@{snap}", dataset_name(parent, clone_id));
    check_snapshot(parent, &full)?;
    run(&["snapshot", &full])?;
    Ok(full)
}

/// `zfs clone -o mountpoint=<HOMES_DIR>/<new-id> <snapshot> <parent>/<new-id>`.
/// Returns the new dataset name.
pub(crate) fn clone_dataset(parent: &str, snapshot: &str, new_id: &str) -> Result<String> {
    check_snapshot(parent, snapshot)?;
    let dst = dataset_name(parent, new_id);
    check_dataset(parent, &dst)?;
    let mp = format!("mountpoint={}", dataset_dir(new_id));
    run(&["clone", "-o", &mp, snapshot, &dst])?;
    Ok(dst)
}

/// `zfs destroy [-r] <parent>/<clone-id>`, then drop the mountpoint directory.
///
/// ZFS creates `<HOMES_DIR>/<clone-id>` to mount the dataset on but does not remove it on
/// destroy, so without this every deleted clone left an empty directory behind in the
/// homes parent — visible for good in `smb://<host>/clones` and in every clone's
/// `~/clones`. `remove_dir` is non-recursive on purpose: it can only succeed on
/// an empty directory, so a destroy that silently left data behind is kept, not deleted.
/// Best-effort — a leftover directory is cosmetic, and failing the delete over one would
/// be worse.
pub(crate) fn destroy(parent: &str, clone_id: &str, recursive: bool) -> Result<()> {
    let ds = dataset_name(parent, clone_id);
    check_dataset(parent, &ds)?;
    if recursive {
        run(&["destroy", "-r", &ds])?;
    } else {
        run(&["destroy", &ds])?;
    }
    if let Err(e) = std::fs::remove_dir(dataset_dir(clone_id)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(target: "zfs", "keeping mountpoint dir for {clone_id}: {e}");
        }
    }
    Ok(())
}

/// `zfs get origin` for a clone's dataset: the snapshot it was cloned from, if any.
/// `None` for a fresh dataset (origin `-`) and on any error. Read-only and unvalidated
/// on purpose — it destroys nothing, so a name outside `parent` can only produce a
/// useless answer, not damage.
/// Reached through [`crate::clone_home::CloneHome::origin`].
pub(crate) fn origin(parent: &str, clone_id: &str) -> Option<String> {
    let ds = dataset_name(parent, clone_id);
    let v = run(&["get", "-H", "-o", "value", "origin", &ds]).ok()?;
    if v.is_empty() || v == "-" {
        None
    } else {
        Some(v)
    }
}

/// `zfs destroy <snapshot>` when no remaining clone dataset was cloned from it.
/// Origin snapshots pair 1:1 with fork clones, so a plain destroy suffices; a busy
/// snapshot (still referenced) surfaces as an error for the caller to keep.
pub(crate) fn destroy_snapshot_if_unreferenced(parent: &str, snapshot: &str) -> Result<()> {
    check_snapshot(parent, snapshot)?;
    run(&["destroy", snapshot])?;
    Ok(())
}

/// `zfs destroy` failed because a mount still holds the dataset (in any namespace).
pub(crate) fn is_busy(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("dataset is busy")
}

/// `zfs destroy` failed because a fork clone still depends on one of the snapshots.
pub(crate) fn has_dependents(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("dependent clones")
}

/// `zfs destroy` failed because the dataset is already gone.
pub(crate) fn is_missing(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("does not exist")
}

/// One dataset directly under the homes parent, as the orphan sweep sees it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HomeDataset {
    /// The clone id: the dataset name with the parent stripped.
    pub id: String,
    /// The `name@snap` it was cloned from, when it is a fork.
    pub origin: Option<String>,
    pub mountpoint: String,
}

/// One snapshot of a dataset directly under the homes parent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HomeSnapshot {
    /// Full `name@snap`.
    pub name: String,
    /// Whether any dataset was cloned from it.
    pub has_clones: bool,
}

/// Every dataset directly under `parent` (`zfs list -d 1`). Read-only.
pub(crate) fn list_homes(parent: &str) -> Result<Vec<HomeDataset>> {
    let out = run(&[
        "list",
        "-H",
        "-p",
        "-d",
        "1",
        "-t",
        "filesystem",
        "-o",
        "name,origin,mountpoint",
        parent,
    ])?;
    Ok(parse_homes(parent, &out))
}

/// Every snapshot of every dataset directly under `parent`. Read-only.
pub(crate) fn list_snapshots(parent: &str) -> Result<Vec<HomeSnapshot>> {
    let out = run(&[
        "list",
        "-H",
        "-p",
        "-r",
        "-d",
        "2",
        "-t",
        "snapshot",
        "-o",
        "name,clones",
        parent,
    ])?;
    Ok(parse_snapshots(parent, &out))
}

fn parse_homes(parent: &str, out: &str) -> Vec<HomeDataset> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let name = f.next()?;
            let origin = f.next()?;
            let mountpoint = f.next()?.trim();
            check_dataset(parent, name).ok()?;
            Some(HomeDataset {
                id: name.strip_prefix(&format!("{parent}/"))?.to_string(),
                origin: (!origin.is_empty() && origin != "-").then(|| origin.to_string()),
                mountpoint: mountpoint.to_string(),
            })
        })
        .collect()
}

fn parse_snapshots(parent: &str, out: &str) -> Vec<HomeSnapshot> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let name = f.next()?;
            let clones = f.next().unwrap_or("").trim();
            check_snapshot(parent, name).ok()?;
            Some(HomeSnapshot {
                name: name.to_string(),
                has_clones: !clones.is_empty() && clones != "-",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "rpool/rmng-homes-101";

    #[test]
    fn homes_listing_skips_the_parent_and_reads_origins() {
        let out = "rpool/rmng-homes-101\t-\t/srv/rmng-homes-101\n\
                   rpool/rmng-homes-101/a\t-\t/srv/rmng-homes/a\n\
                   rpool/rmng-homes-101/b\trpool/rmng-homes-101/a@fork-b-1\t/srv/rmng-homes/b\n";
        let got = parse_homes(P, out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "a");
        assert_eq!(got[0].origin, None);
        assert_eq!(got[1].id, "b");
        assert_eq!(
            got[1].origin.as_deref(),
            Some("rpool/rmng-homes-101/a@fork-b-1")
        );
    }

    #[test]
    fn homes_listing_never_yields_a_dataset_outside_the_parent() {
        let out = "rpool/data/subvol-204-disk-0\t-\t/x\n\
                   rpool/rmng-homes-1010/a\t-\t/srv/rmng-homes/a\n\
                   rpool/rmng-homes-101/a/deeper\t-\t/y\n";
        assert!(parse_homes(P, out).is_empty());
    }

    #[test]
    fn snapshot_listing_reads_clones() {
        let out = "rpool/rmng-homes-101/a@fork-b-1\trpool/rmng-homes-101/b\n\
                   rpool/rmng-homes-101/a@fork-c-2\t\n\
                   rpool/rmng-homes-101/a@fork-d-3\t-\n";
        let got = parse_snapshots(P, out);
        assert_eq!(got.len(), 3);
        assert!(got[0].has_clones);
        assert!(!got[1].has_clones);
        assert!(!got[2].has_clones);
    }

    #[test]
    fn destroy_errors_are_classified() {
        let busy = anyhow::anyhow!("zfs destroy -r x failed: cannot destroy 'x': dataset is busy");
        let deps = anyhow::anyhow!(
            "zfs destroy -r x failed: cannot destroy 'x@s': snapshot has dependent clones"
        );
        assert!(is_busy(&busy) && !has_dependents(&busy));
        assert!(has_dependents(&deps) && !is_busy(&deps));
    }
}
