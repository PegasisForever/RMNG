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

/// Direct filesystem IO into a clone's live home (the merged view). The server holds
/// these mounts itself from clone creation to deletion, so for files under `/home/rmng`
/// this replaces daemon tar roundtrips: plain reads/writes the clone sees instantly, with
/// no guest shell and no archive parsing. `rel` is the home-relative path
/// (`".claude.json"`, `".codex/auth.json"`). Files outside the home bind (e.g. the
/// `/etc/rmng` stamps) still go through the daemon.
///
/// `homes` is a parameter (not the [`crate::zfs::HOMES_DIR`] constant) so tests can point
/// at a scratch dir.
/// Read one home-relative file. `None` when missing — an explicit absence branch.
pub fn read_home_file(homes: &Path, id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
    let path = homes.join(MERGED_DIR).join(id).join(rel);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {} in {id}'s home", path.display())),
    }
}

/// Write one home-relative file atomically (temp + rename in the same dir), mode 0600,
/// owned by the clone user — the same landing the old tar uploads gave. Missing parents
/// are created and the whole chain chowned, so a file never lands under a root-owned
/// dir its agent cannot write beside (the phase-30 lesson).
pub fn write_home_file(homes: &Path, id: &str, rel: &str, data: &[u8], mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = homes.join(MERGED_DIR).join(id);
    let path = root.join(rel);
    let parent = path.parent().with_context(|| format!("no parent for home path {rel:?}"))?;
    // Own the full chain: only missing components are created, but every component down
    // to the merged root is chowned — deterministic no matter who made the dir.
    let mut dir = root.clone();
    chown(&dir)?;
    if let Ok(rel_parent) = parent.strip_prefix(&root) {
        for comp in rel_parent.components() {
            dir.push(comp);
            if !dir.exists() {
                std::fs::create_dir(&dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            chown(&dir)?;
        }
    }
    let tmp = parent.join(".rmng-write.tmp");
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, PermissionsExt::from_mode(mode))?;
    chown(&tmp)?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("installing {} in {id}'s home", path.display()))?;
    Ok(())
}

fn chown(path: &Path) -> Result<()> {
    std::os::unix::fs::chown(path, Some(1000), Some(1000))
        .with_context(|| format!("chowning {}", path.display()))
}

/// Delete one home-relative file (`rm -f` semantics: missing is fine).
pub fn remove_home_file(homes: &Path, id: &str, rel: &str) -> Result<()> {
    let path = homes.join(MERGED_DIR).join(id).join(rel);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {} from {id}'s home", path.display())),
    }
}

/// Ensure a home-relative dir exists with an exact mode, owned by the clone user.
/// sshd's `StrictModes` refuses `authorized_keys` under a group/world-writable `.ssh`,
/// so that dir goes through here (0700) rather than the default-mode parents
/// [`write_home_file`] makes.
pub fn ensure_home_dir(homes: &Path, id: &str, rel: &str, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = homes.join(MERGED_DIR).join(id).join(rel);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {} in {id}'s home", dir.display()))?;
    std::fs::set_permissions(&dir, PermissionsExt::from_mode(mode))
        .with_context(|| format!("chmodding {} in {id}'s home", dir.display()))?;
    chown(&dir)
}

/// Create a home-relative symlink (unit masks: link → `/dev/null`). Parents are ensured
/// like [`write_home_file`]. Any existing file/symlink at the path is replaced; an
/// existing dir is a hard error. Link ownership is deliberately left to the process
/// (root on the CT): the kernel ignores symlink ownership on resolution, so the old
/// tar's 0:0 was incidental, not load-bearing.
pub fn write_home_symlink(homes: &Path, id: &str, rel: &str, target: &str) -> Result<()> {
    let root = homes.join(MERGED_DIR).join(id);
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {} in {id}'s home", parent.display()))?;
    }
    match std::fs::symlink_metadata(&path) {
        Ok(md) if md.file_type().is_dir() => {
            anyhow::bail!("refusing to replace dir {} in {id}'s home", path.display())
        }
        Ok(_) => std::fs::remove_file(&path)
            .with_context(|| format!("removing {} in {id}'s home", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stating {} in {id}'s home", path.display())),
    }
    std::os::unix::fs::symlink(target, &path)
        .with_context(|| format!("linking {} in {id}'s home", path.display()))?;
    Ok(())
}

/// This server's live homes root.
fn live_homes() -> &'static Path {
    Path::new(crate::zfs::HOMES_DIR)
}

/// Whether clone `id` has a live home (merged view mounted). False for deleted clones
/// (mount torn down) — the push passes skip those the way they used to skip stopped
/// containers, while stopped-but-existing clones now take the push.
pub fn clone_home_present(id: &str) -> bool {
    live_homes().join(MERGED_DIR).join(id).is_dir()
}

/// [`read_home_file`] against this server's homes.
pub fn read_clone_home(id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
    read_home_file(live_homes(), id, rel)
}

/// [`write_home_file`] against this server's homes.
pub fn write_clone_home(id: &str, rel: &str, data: &[u8], mode: u32) -> Result<()> {
    write_home_file(live_homes(), id, rel, data, mode)
}

/// [`ensure_home_dir`] against this server's homes.
pub fn ensure_clone_home_dir(id: &str, rel: &str, mode: u32) -> Result<()> {
    ensure_home_dir(live_homes(), id, rel, mode)
}

/// [`remove_home_file`] against this server's homes.
pub fn remove_clone_home(id: &str, rel: &str) -> Result<()> {
    remove_home_file(live_homes(), id, rel)
}

/// [`write_home_symlink`] against this server's homes.
pub fn symlink_clone_home(id: &str, rel: &str, target: &str) -> Result<()> {
    write_home_symlink(live_homes(), id, rel, target)
}

/// Lowerdir currently mounted at `merged`, if it is an overlay mount. A mountinfo line
/// naming our mountpoint without a parseable lowerdir is an error, not an unmounted
/// verdict: remounting blind over it risks EBUSY and hides a world we do not understand.
fn mounted_lower(merged: &Path) -> Result<Option<String>> {
    let info = std::fs::read_to_string("/proc/self/mountinfo")
        .context("reading /proc/self/mountinfo")?;
    let wanted = merged.to_string_lossy();
    let mut found: Option<&str> = None;
    for line in info.lines() {
        if let Some((fields, _)) = line.split_once(" - ") {
            if fields.split_whitespace().nth(4) == Some(wanted.as_ref()) {
                found = Some(line);
                break;
            }
        }
    }
    found.map(|line| lower_of_line_strict(line, &wanted)).transpose()
}

/// Parse one mountinfo line: the mount point is the 5th pre-separator field, the
/// lowerdir hides in the comma-separated super options after it. Pure so tests can
/// pin the shape without mounting anything. Strict: the caller only passes lines already
/// matched on mountpoint, so anything unparseable here is an error.
fn lower_of_line_strict(line: &str, wanted: &str) -> Result<String> {
    let (fields, after) = line
        .split_once(" - ")
        .with_context(|| format!("mountinfo line without separator: {line}"))?;
    let point = fields.split_whitespace().nth(4).unwrap_or("<short>");
    if point != wanted {
        anyhow::bail!("mountinfo line for {point} reached the strict parser for {wanted}");
    }
    after
        .split_whitespace()
        .nth(2)
        .with_context(|| format!("mountinfo line without super options: {line}"))?
        .split(',')
        .find_map(|o| o.strip_prefix("lowerdir=").map(str::to_string))
        .with_context(|| format!("overlay mount without lowerdir: {line}"))
}

/// Unpack an image-home tar into `dest`, stripping the single top-level directory the
/// daemon wraps entries in. Dotfiles survive (read_dir yields them); ownership stays as
/// archived, matching the image — `set_preserve_ownerships` is load-bearing here, the
/// tar crate otherwise unpacks everything as the server's own uid (root) and the clone
/// user cannot read its own home (found live: every unit stayed inactive, no Hello).
fn unpack_skeleton(tar_bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(tar_bytes);
    archive.set_preserve_ownerships(true);
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
    // NB: mount paths come from HOMES_DIR (the mountpoint), not the dataset name.
    let dest = skeleton_dir(crate::zfs::HOMES_DIR, &digest);
    // Marker carries a version: v1 skeletons were exported WITHOUT ownership (tar-crate
    // default) and must be re-exported, not trusted. Bump on any export-format change.
    let marker = dest.join(SKEL_MARKER);
    if std::fs::read_to_string(&marker)
        .map(|s| s.trim() == format!("v2:{digest}"))
        .unwrap_or(false)
    {
        return Ok(digest);
    }
    tracing::info!(target: "overlay", "exporting {IMAGE_HOME} of {image_tag} for the home overlay");
    // A stale dir (old marker) may hold wrongly-owned files the unpack would merge with:
    // wipe it so the export is exactly the image, not image-over-leftovers.
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("clearing stale skeleton {}", dest.display()))?;
    }
    std::fs::create_dir_all(&dest).with_context(|| format!("mkdir {}", dest.display()))?;
    // Unique per attempt: a previous export that died between create and remove leaves
    // its reader behind, and a deterministic name would 409 the retry on it. Pre-epoch
    // clocks do not exist; fail loudly instead of reusing a colliding `0`.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before the epoch")
        .as_nanos();
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
    std::fs::write(&marker, format!("v2:{digest}\n")).context("writing skeleton marker")?;
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
    match mounted_lower(merged)? {
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
        .map(|h| {
            let dataset = h
                .dataset
                .clone()
                .expect("managed clone passed the is_some filter without a dataset");
            (h.id, dataset, h.base_tag)
        })
        .collect();
    // Mount paths come from HOMES_DIR (the mountpoint), never the dataset name.
    let homes = crate::zfs::HOMES_DIR;
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
            ensure_mounted(Path::new(dataset), &digest, &merged_dir(homes, id)).await
        {
            tracing::warn!(target: "overlay", "remount: {id}: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_homes(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rmng-hometest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".merged").join("c1")).unwrap();
        dir
    }

    #[test]
    fn home_roundtrip_missing_read_and_remove_are_none_and_ok() {
        let homes = scratch_homes("roundtrip");
        assert_eq!(read_home_file(&homes, "c1", ".codex/auth.json").unwrap(), None);
        remove_home_file(&homes, "c1", ".codex/auth.json").unwrap();
        write_home_file(&homes, "c1", ".codex/auth.json", b"{\"a\":1}", 0o600).unwrap();
        assert_eq!(
            read_home_file(&homes, "c1", ".codex/auth.json").unwrap().as_deref(),
            Some(b"{\"a\":1}".as_slice())
        );
        remove_home_file(&homes, "c1", ".codex/auth.json").unwrap();
        assert_eq!(read_home_file(&homes, "c1", ".codex/auth.json").unwrap(), None);
        let _ = std::fs::remove_dir_all(&homes);
    }

    #[test]
    fn home_write_creates_parents_and_lands_0600() {
        use std::os::unix::fs::PermissionsExt;
        let homes = scratch_homes("parents");
        write_home_file(&homes, "c1", ".pi/agent/auth.json", b"{}", 0o600).unwrap();
        let path = homes.join(".merged").join("c1").join(".pi/agent/auth.json");
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // No temp droppings beside the installed file.
        assert!(std::fs::read_dir(path.parent().unwrap()).unwrap().count() == 1);
        let _ = std::fs::remove_dir_all(&homes);
    }

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
            lower_of_line_strict(line, "/srv/rmng-homes/.merged/pega-x").unwrap(),
            "/srv/rmng-homes/.skeleton/sha256-ab".to_string()
        );
        assert!(lower_of_line_strict(line, "/nope").is_err());
    }
}
