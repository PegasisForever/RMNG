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
//! module — no raw `zfs destroy` elsewhere.
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
pub fn dataset_dir(clone_id: &str) -> String {
    format!("{HOMES_DIR}/{clone_id}")
}

/// Dataset name for a clone id under `parent`.
pub fn dataset_name(parent: &str, clone_id: &str) -> String {
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
pub fn create(parent: &str, clone_id: &str) -> Result<()> {
    let ds = dataset_name(parent, clone_id);
    check_dataset(parent, &ds)?;
    let mp = format!("mountpoint={}", dataset_dir(clone_id));
    run(&["create", "-o", &mp, &ds])?;
    Ok(())
}

/// `zfs snapshot <parent>/<clone-id>@<snap>`.
pub fn snapshot(parent: &str, clone_id: &str, snap: &str) -> Result<String> {
    let full = format!("{}@{snap}", dataset_name(parent, clone_id));
    check_snapshot(parent, &full)?;
    run(&["snapshot", &full])?;
    Ok(full)
}

/// `zfs clone -o mountpoint=<HOMES_DIR>/<new-id> <snapshot> <parent>/<new-id>`.
/// Returns the new dataset name.
pub fn clone_dataset(parent: &str, snapshot: &str, new_id: &str) -> Result<String> {
    check_snapshot(parent, snapshot)?;
    let dst = dataset_name(parent, new_id);
    check_dataset(parent, &dst)?;
    let mp = format!("mountpoint={}", dataset_dir(new_id));
    run(&["clone", "-o", &mp, snapshot, &dst])?;
    Ok(dst)
}

/// `zfs destroy [-r] <parent>/<clone-id>`.
pub fn destroy(parent: &str, clone_id: &str, recursive: bool) -> Result<()> {
    let ds = dataset_name(parent, clone_id);
    check_dataset(parent, &ds)?;
    if recursive {
        run(&["destroy", "-r", &ds])?;
    } else {
        run(&["destroy", &ds])?;
    }
    Ok(())
}

/// `zfs destroy <snapshot>` when no remaining clone dataset was cloned from it.
/// Origin snapshots pair 1:1 with fork clones, so a plain destroy suffices; a busy
/// snapshot (still referenced) surfaces as an error for the caller to keep.
pub fn destroy_snapshot_if_unreferenced(parent: &str, snapshot: &str) -> Result<()> {
    check_snapshot(parent, snapshot)?;
    run(&["destroy", snapshot])?;
    Ok(())
}
