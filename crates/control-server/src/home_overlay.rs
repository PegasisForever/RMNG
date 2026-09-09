//! Per-clone home as an overlay mount: the template's home is the shared read-only
//! lower, the clone's ZFS dataset holds the read-write upper, and the merged view is
//! what binds at `/home/rmng`.
//!
//! Why not just the dataset: a fresh dataset is empty, and the bind would shadow the
//! whole template-provided home layer (user units, toolchains, default configs). The
//! overlay keeps the template the single source — a rebase swaps the lower under the
//! same upper, so user files persist while the base refreshes — with instant,
//! copy-free fresh clones and forks at any skeleton size.
//!
//! Layout (all under the homes dir, inside the existing `:shared` bind, so mounts made
//! here propagate to the host mount namespace where the Docker daemon resolves binds):
//! - `<homes>/.skeleton/<digest>/` — exported `/home/rmng` of one image (lower).
//!   Keyed by image id, exported once per template build; shared by every clone on it.
//! - `<dataset>/upper` + `<dataset>/work` — the clone's delta (upper must not be the
//!   dataset root: overlayfs needs its workdir on the same filesystem but outside it).
//! - `<homes>/.merged/<id>` — the merged view, bound at `/home/rmng`.
//!
//! Mounts die with a CT reboot (not with container stop/remove — host mounts outlive
//! containers), so boot re-establishes them ([`remount_all`]). No skeleton GC yet: one
//! copy per published template; noted for later.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::app::App;

const SKEL_DIR: &str = ".skeleton";
const MERGED_DIR: &str = ".merged";
const UPPER_DIR: &str = "upper";
const WORK_DIR: &str = "work";
const SKEL_MARKER: &str = ".rmng-skeleton";
const IMAGE_HOME: &str = "/home/rmng";

/// Filesystem-safe form of an image id (`sha256:…` → `sha256-…`).
fn digest_path(digest: &str) -> String {
    digest
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Exported home of one image (overlay lower).
pub fn skeleton_dir(homes: &str, digest: &str) -> PathBuf {
    Path::new(homes)
        .join(SKEL_DIR)
        .join(digest_path(digest))
}

/// The clone's read-write delta inside its dataset.
pub fn upper_dir(dataset: &Path) -> PathBuf {
    dataset.join(UPPER_DIR)
}

/// Overlay workdir: same filesystem as the upper, outside it.
fn work_dir(dataset: &Path) -> PathBuf {
    dataset.join(WORK_DIR)
}

/// The merged view bound at `/home/rmng`.
pub fn merged_dir(homes: &str, id: &str) -> PathBuf {
    Path::new(homes).join(MERGED_DIR).join(id)
}

/// Lowerdir currently mounted at `merged`, if it is an overlay mount.
fn mounted_lower(merged: &Path) -> Option<String> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let wanted = merged.to_string_lossy();
    info.lines().find_map(|line| lower_of_line(line, &wanted))
}

/// Parse one mountinfo line: the mount point is the 5th pre-separator field, the
/// lowerdir hides in the comma-separated super options after it. Pure so tests can
/// pin the shape without mounting anything.
fn lower_of_line(line: &str, wanted: &str) -> Option<String> {
    let (fields, after) = line.split_once(" - ")?;
    if fields.split_whitespace().nth(4)? != wanted {
        return None;
    }
    after
        .split_whitespace()
        .nth(2)?
        .split(',')
        .find_map(|o| o.strip_prefix("lowerdir=").map(str::to_string))
}

/// Unpack an image-home tar into `dest`, stripping the single top-level directory the
/// daemon wraps entries in. Dotfiles survive (read_dir yields them); ownership stays as
/// archived, matching the image.
fn unpack_skeleton(tar_bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(tar_bytes);
    for entry in archive.entries().context("reading skeleton tar")? {
        let mut entry = entry.context("skeleton tar entry")?;
        let path = entry.path().context("skeleton entry path")?.into_owned();
        let mut parts = path.components();
        parts.next(); // strip the top-level dir
        let rest: PathBuf = parts.collect();
        if rest.as_os_str().is_empty() {
            continue;
        }
        entry
            .unpack(dest.join(rest))
            .context("unpacking skeleton entry")?;
    }
    Ok(())
}

/// Export `/home/rmng` of `image_tag` into the shared skeleton dir, once per image id.
/// Returns the digest. Best-effort callers treat failure as fatal: without a lower there
/// is no home to mount.
pub async fn ensure_skeleton(app: &App, image_tag: &str) -> Result<String> {
    let digest = app.docker.image_id(image_tag).await?;
    let dest = skeleton_dir(&app.config().docker.homes_parent, &digest);
    let marker = dest.join(SKEL_MARKER);
    if std::fs::read_to_string(&marker)
        .map(|s| s.trim() == digest)
        .unwrap_or(false)
    {
        return Ok(digest);
    }
    tracing::info!(target: "overlay", "exporting {IMAGE_HOME} of {image_tag} for the home overlay");
    std::fs::create_dir_all(&dest).with_context(|| format!("mkdir {}", dest.display()))?;
    // Unique per attempt: a previous export that died between create and remove leaves
    // its reader behind, and a deterministic name would 409 the retry on it.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let reader = app
        .docker
        .create_reader(
            image_tag,
            &format!("rmng-skel-{}-{}-{unique}", digest_path(&digest), std::process::id()),
        )
        .await?;
    let tar = app.docker.download_home_tar(&reader, IMAGE_HOME).await;
    let _ = app.docker.remove_container(&reader).await;
    let tar = tar?;
    unpack_skeleton(&tar, &dest)?;
    std::fs::write(&marker, format!("{digest}\n")).context("writing skeleton marker")?;
    Ok(digest)
}

/// Make sure a dataset holds the overlay upper + work dirs.
pub fn ensure_layout(dataset: &Path) -> Result<()> {
    std::fs::create_dir_all(upper_dir(dataset))
        .with_context(|| format!("mkdir {}", upper_dir(dataset).display()))?;
    std::fs::create_dir_all(work_dir(dataset))
        .with_context(|| format!("mkdir {}", work_dir(dataset).display()))?;
    Ok(())
}

fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<()> {
    std::fs::create_dir_all(merged)
        .with_context(|| format!("mkdir {}", merged.display()))?;
    let opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    );
    nix::mount::mount(
        Some("overlay"),
        merged,
        Some("overlay"),
        nix::mount::MsFlags::empty(),
        Some(opts.as_str()),
    )
    .with_context(|| format!("mounting overlay at {}", merged.display()))?;
    Ok(())
}

/// Best-effort unmount (lazy detach: open files — tails, smbd — keep working). Missing
/// mount is fine.
pub fn unmount_merged(merged: &Path) {
    match nix::mount::umount2(merged, nix::mount::MntFlags::MNT_DETACH) {
        Ok(()) => {}
        Err(nix::errno::Errno::EINVAL) => {} // not mounted
        Err(e) => tracing::warn!(target: "overlay", "unmounting {}: {e}", merged.display()),
    }
}

/// Tear down one clone's merged view (delete path): unmount, then remove the dir so no
/// dangling mountpoint survives. The dataset itself is the caller's business.
pub fn teardown_merged(homes: &str, id: &str) {
    let merged = merged_dir(homes, id);
    unmount_merged(&merged);
    let _ = std::fs::remove_dir(&merged);
}

/// Mount (or remount, when the lower changed, e.g. rebase) one clone's home overlay.
/// Idempotent: an already-correct mount is left alone.
pub async fn ensure_mounted(dataset: &Path, digest: &str, merged: &Path) -> Result<()> {
    ensure_layout(dataset)?;
    let homes = merged
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow!("merged path has no homes parent: {}", merged.display()))?;
    let lower = skeleton_dir(&homes.to_string_lossy(), digest);
    if !lower.is_dir() {
        anyhow::bail!("skeleton missing for {digest} (export it first)");
    }
    match mounted_lower(merged) {
        Some(cur) if cur.contains(&digest_path(digest)) => return Ok(()),
        Some(_) => unmount_merged(merged),
        None => {}
    }
    mount_overlay(&lower, &upper_dir(dataset), &work_dir(dataset), merged)
}

/// Re-establish every managed clone's overlay after a reboot (mounts do not survive
/// one; containers do not need to run). Best-effort per clone; a missing image warns.
pub async fn remount_all(app: App) {
    let rows: Vec<(String, String, Option<String>)> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && h.dataset.is_some())
        .map(|h| (h.id, h.dataset.unwrap_or_default(), h.base_tag))
        .collect();
    let homes = app.config().docker.homes_parent.clone();
    for (id, dataset, tag) in &rows {
        let Some(tag) = tag.as_deref().filter(|t| !t.trim().is_empty()) else {
            tracing::warn!(target: "overlay", "remount: {id} has no recorded image; skipping");
            continue;
        };
        let digest = match ensure_skeleton(&app, tag).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(target: "overlay", "remount: skeleton for {id}: {e:#}");
                continue;
            }
        };
        if let Err(e) =
            ensure_mounted(Path::new(dataset), &digest, &merged_dir(&homes, id)).await
        {
            tracing::warn!(target: "overlay", "remount: {id}: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_path_keeps_it_a_single_safe_component() {
        assert_eq!(digest_path("sha256:abc123"), "sha256-abc123");
        assert!(!digest_path("sha256:ab/c").contains('/'));
    }

    #[test]
    fn layout_paths_nest_under_homes_and_dataset() {
        let ds = Path::new("/srv/rmng-homes/pega-x");
        assert_eq!(upper_dir(ds), Path::new("/srv/rmng-homes/pega-x/upper"));
        assert_eq!(
            merged_dir("/srv/rmng-homes", "pega-x"),
            Path::new("/srv/rmng-homes/.merged/pega-x")
        );
        assert_eq!(
            skeleton_dir("/srv/rmng-homes", "sha256:ab"),
            Path::new("/srv/rmng-homes/.skeleton/sha256-ab")
        );
    }

    const MOUNTINFO: &str = "\
30 1 0:27 / /proc rw shared:12 - proc proc rw
55 1 0:60 / /srv/rmng-homes/.merged/pega-x rw shared:90 - overlay overlay rw,lowerdir=/srv/rmng-homes/.skeleton/sha256-ab,upperdir=/srv/rmng-homes/pega-x/upper,workdir=/srv/rmng-homes/pega-x/work
60 1 0:61 / /data rw - ext4 /dev/sda1 rw";

    #[test]
    fn mounted_lower_reads_the_overlay_options() {
        let line = MOUNTINFO.lines().nth(1).unwrap();
        assert_eq!(
            lower_of_line(line, "/srv/rmng-homes/.merged/pega-x"),
            Some("/srv/rmng-homes/.skeleton/sha256-ab".to_string())
        );
        assert_eq!(lower_of_line(line, "/nope"), None);
    }
}
