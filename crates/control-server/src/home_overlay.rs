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
//!
//! This is the implementation of the overlay, not the interface to a clone's home:
//! [`crate::clone_home::CloneHome`] owns the path derivations below and is what callers
//! hold. The exceptions are the direct home-file IO (`read_clone_home` and friends),
//! which is addressed by id and home-relative path and belongs to no single home, and
//! [`remount_all`], the boot pass over all of them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::app::App;
use crate::clone_home::CloneHome;

const SKEL_DIR: &str = ".skeleton";
const MERGED_DIR: &str = ".merged";
const UPPER_DIR: &str = "upper";
const WORK_DIR: &str = "work";
const SKEL_MARKER: &str = ".rmng-skeleton";
const IMAGE_HOME: &str = "/home/rmng";

/// One in-flight skeleton export per image digest (see [`ensure_skeleton`]).
static SKEL_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(Default::default);

fn skeleton_lock(digest: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    SKEL_LOCKS
        .lock()
        .unwrap()
        .entry(digest.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

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
fn skeleton_dir(homes: &str, digest: &str) -> PathBuf {
    Path::new(homes).join(SKEL_DIR).join(digest_path(digest))
}

/// The clone's read-write delta inside its dataset.
/// Reached through [`CloneHome::upper`].
pub(crate) fn upper_dir(dataset: &Path) -> PathBuf {
    dataset.join(UPPER_DIR)
}

/// Overlay workdir: same filesystem as the upper, outside it.
/// Reached through [`CloneHome::work`].
pub(crate) fn work_dir(dataset: &Path) -> PathBuf {
    dataset.join(WORK_DIR)
}

/// The directory holding every clone's merged view, one entry per clone with a live
/// home. Bound into every clone at `/clones` (see [`crate::docker::CreateSpec::browse_root`])
/// and the target of every `<data_dir>/hosts` link the SMB share serves, so all three
/// browse paths show one directory.
pub(crate) fn merged_root(homes: &str) -> PathBuf {
    Path::new(homes).join(MERGED_DIR)
}

/// The merged view bound at `/home/rmng`.
/// Reached through [`CloneHome::merged`].
pub(crate) fn merged_dir(homes: &str, id: &str) -> PathBuf {
    merged_root(homes).join(id)
}

/// The two directories a clone browses that are NOT its own files, and where each is
/// really mounted. Both mounts sit OUTSIDE the home on purpose: GNOME's file manager
/// puts a sidebar row on every mount whose path is under the home directory, and each
/// sibling home is its own overlay mount — so mounting the browse root inside the home
/// gave a clone one sidebar row per clone in the fleet. Measured in a CT 204 clone: a
/// mount under the home is listed, the same mount outside it is not.
///
/// [`ensure_home_links`] keeps `~/clones` and `~/shared` working as symlinks to these,
/// so nothing that uses the familiar paths has to change.
const HOME_LINKS: [(&str, &str); 2] = [("clones", "/clones"), ("shared", "/shared")];

/// Point `~/clones` and `~/shared` at the mounts outside the home. Idempotent, and safe
/// on a home that has never seen them.
///
/// `home` is the merged view — [`CloneHome::merged`] — not a homes root plus an id:
/// this module no longer derives that path for anyone.
pub(crate) fn ensure_home_links(home: &Path, id: &str) {
    for (name, target) in HOME_LINKS {
        let path = home.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(md) if md.file_type().is_symlink() => {
                if std::fs::read_link(&path).is_ok_and(|t| t == Path::new(target)) {
                    continue;
                }
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(target: "overlay", "{id}: keeping ~/{name}: {e}");
                    continue;
                }
            }
            Ok(_) => {
                tracing::warn!(target: "overlay", "{id}: ~/{name} is not a symlink, leaving it");
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(target: "overlay", "{id}: reading ~/{name}: {e}");
                continue;
            }
        }
        if let Err(e) = std::os::unix::fs::symlink(target, &path) {
            tracing::warn!(target: "overlay", "{id}: linking ~/{name} -> {target}: {e}");
            continue;
        }
        // The link itself belongs to the clone user, like everything else in the home.
        // Traversal reads the TARGET's permissions, so this is tidiness, not access.
        let _ = std::os::unix::fs::lchown(
            &path,
            Some(crate::shared::CLONE_UID),
            Some(crate::shared::CLONE_UID),
        );
    }
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
    let parent = path
        .parent()
        .with_context(|| format!("no parent for home path {rel:?}"))?;
    // Own the full chain: only missing components are created, but every component down
    // to the merged root is chowned — deterministic no matter who made the dir.
    let mut dir = root.clone();
    chown(&dir)?;
    if let Ok(rel_parent) = parent.strip_prefix(&root) {
        for comp in rel_parent.components() {
            dir.push(comp);
            if !dir.exists() {
                std::fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
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

/// Every user-data dir a Chromium-family browser might keep a profile lock in. Only
/// `google-chrome` exists on the fleet today; the rest cost one array entry each and
/// save a second diagnosis when someone installs Edge.
const BROWSER_PROFILE_DIRS: [&str; 5] = [
    "google-chrome",
    "google-chrome-beta",
    "google-chrome-unstable",
    "chromium",
    "microsoft-edge",
];

/// The three entries Chromium's `ProcessSingleton` keeps at the root of a user-data dir.
/// `SingletonLock` is the one that matters; the other two are removed with it because
/// that is what Chrome itself does when it breaks a lock, and a cookie left pointing at
/// a socket that no longer exists is only a second thing to explain.
const BROWSER_LOCK_FILES: [&str; 3] = ["SingletonLock", "SingletonCookie", "SingletonSocket"];

/// Drop every stale Chromium profile lock from a clone's home.
///
/// Chrome records its lock as a SYMLINK whose target is the string `<hostname>-<pid>`,
/// at the root of the user-data dir — on-disk state, inside the home. That is fine until
/// the home moves: a gen-2 fork ZFS-clones the source's home verbatim and boots it under
/// a NEW hostname, so the fork comes up owning a lock that names the source. Chrome will
/// break a stale lock naming its OWN host (it checks whether the pid is alive), but it
/// cannot check a pid on another machine, so a foreign hostname makes it refuse to start
/// at all — `DisplayProfileInUseError`, a zenity "Unlock Profile and Relaunch" dialog and
/// no browser. Hostname mismatch is the whole trigger; pid staleness never enters into it.
///
/// The same lock also rides in from a template home that had Chrome run in it, which is
/// how every clone on one production CT ended up unable to open Chrome with no fork
/// anywhere in their history. So this is not fork cleanup: it belongs on every path that
/// brings a home up under a hostname, which is why it runs pre-boot for all of them.
///
/// Removal is unconditional because the caller holds a STOPPED container: nothing in it
/// can be holding a lock legitimately. Errors are swallowed per entry — a home without a
/// browser profile is the common case, and a lock we could not remove is a browser that
/// will not start, never a clone that must not boot.
pub fn clear_browser_profile_locks(homes: &Path, id: &str) {
    for dir in BROWSER_PROFILE_DIRS {
        for file in BROWSER_LOCK_FILES {
            let _ = remove_home_file(homes, id, &format!(".config/{dir}/{file}"));
        }
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
        Err(e) => {
            return Err(e).with_context(|| format!("stating {} in {id}'s home", path.display()));
        }
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

/// [`clear_browser_profile_locks`] against this server's homes.
pub fn clear_clone_browser_profile_locks(id: &str) {
    clear_browser_profile_locks(live_homes(), id)
}

/// [`write_home_symlink`] against this server's homes.
pub fn symlink_clone_home(id: &str, rel: &str, target: &str) -> Result<()> {
    write_home_symlink(live_homes(), id, rel, target)
}

/// Lowerdir currently mounted at `merged`, if it is an overlay mount. A mountinfo line
/// naming our mountpoint without a parseable lowerdir is an error, not an unmounted
/// verdict: remounting blind over it risks EBUSY and hides a world we do not understand.
fn mounted_lower(merged: &Path) -> Result<Option<String>> {
    let info =
        std::fs::read_to_string("/proc/self/mountinfo").context("reading /proc/self/mountinfo")?;
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
    found
        .map(|line| lower_of_line_strict(line, &wanted))
        .transpose()
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
pub(crate) async fn ensure_skeleton(app: &App, image_tag: &str) -> Result<String> {
    let digest = app.docker.image_id(image_tag).await?;
    // One export per digest at a time. Every clone of one preset resolves to the SAME
    // image, so concurrent migrations all land here together — and the body below wipes
    // the directory before re-exporting, which a second caller would read mid-wipe.
    // Mirrors `derived::BUILD_LOCKS`.
    let lock = skeleton_lock(&digest);
    let _guard = lock.lock().await;
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
            &format!(
                "rmng-skel-{}-{}-{unique}",
                digest_path(&digest),
                std::process::id()
            ),
        )
        .await?;
    let tar = app.docker.download_home_tar(&reader, IMAGE_HOME).await;
    let _ = app.docker.remove_container(&reader).await;
    let tar = tar?;
    unpack_skeleton(&tar, &dest)?;
    std::fs::write(&marker, format!("v2:{digest}\n")).context("writing skeleton marker")?;
    Ok(digest)
}

fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<()> {
    std::fs::create_dir_all(merged).with_context(|| format!("mkdir {}", merged.display()))?;
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
fn unmount_merged(merged: &Path) {
    match nix::mount::umount2(merged, nix::mount::MntFlags::MNT_DETACH) {
        Ok(()) => {}
        Err(nix::errno::Errno::EINVAL) => {} // not mounted
        Err(e) => tracing::warn!(target: "overlay", "unmounting {}: {e}", merged.display()),
    }
}

/// Tear down one clone's merged view (delete path): unmount, then remove the dir so no
/// dangling mountpoint survives. The dataset itself is the caller's business — see
/// [`CloneHome::teardown`], which is how this is reached.
pub(crate) fn teardown_merged(merged: &Path) {
    unmount_merged(merged);
    let _ = std::fs::remove_dir(merged);
}

/// Mount (or remount, when the lower changed, e.g. rebase) one clone's home overlay.
/// Idempotent: an already-correct mount is left alone. The upper and work dirs must
/// already exist ([`CloneHome::ensure_layout`]) — the migration fills the upper long
/// before there is an image to mount over it, so ensuring them here too would be a
/// second owner for one rule.
/// Returns `true` when this call ESTABLISHED the mount (as opposed to finding it already
/// correct). A container bound before that moment captured the bare mountpoint and needs
/// restarting — see [`remount_all`].
pub(crate) async fn ensure_mounted(dataset: &Path, digest: &str, merged: &Path) -> Result<bool> {
    let homes = merged
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow!("merged path has no homes parent: {}", merged.display()))?;
    let lower = skeleton_dir(&homes.to_string_lossy(), digest);
    if !lower.is_dir() {
        anyhow::bail!("skeleton missing for {digest} (export it first)");
    }
    match mounted_lower(merged)? {
        Some(cur) if cur.contains(&digest_path(digest)) => return Ok(false),
        Some(_) => unmount_merged(merged),
        None => {}
    }
    mount_overlay(&lower, &upper_dir(dataset), &work_dir(dataset), merged)?;
    Ok(true)
}

/// Re-establish every managed clone's overlay after a reboot (mounts do not survive
/// one; containers do not need to run). Best-effort per clone; a missing image warns.
///
/// Fleet-wide, so it lives here rather than on [`CloneHome`]: it builds one home per
/// managed gen-2 row and asks each to come up. No path is derived in this function.
pub(crate) async fn remount_all(app: App) {
    let mut rows: Vec<(CloneHome, Option<String>)> = Vec::new();
    for h in app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && crate::clone_home::is_gen2(h))
    {
        rows.push((CloneHome::of(&app, &h.id), h.base_tag));
    }
    // Clones whose overlay this pass established: their containers, if already running,
    // are bound to the bare mountpoint and must be restarted.
    let mut remounted: Vec<String> = Vec::new();
    for (home, tag) in &rows {
        let id = home.id();
        // Dataset first, overlay second. A CT reboot leaves the per-clone datasets
        // UNMOUNTED (see [`CloneHome::ensure_mounted`]), and building the overlay on an
        // unmounted dataset stacks it on an empty directory — the clone then gets a
        // pristine template home while its real one sits there unmounted.
        if let Err(e) = home.ensure_mounted() {
            tracing::warn!(target: "overlay", "remount: mounting the dataset for {id}: {e:#}");
            continue;
        }
        let Some(tag) = tag.as_deref().filter(|t| !t.trim().is_empty()) else {
            tracing::warn!(target: "overlay", "remount: {id} has no recorded image; skipping");
            continue;
        };
        match home.ensure_overlay(&app, tag).await {
            // Mounted just now: a container Docker already started bound the bare
            // mountpoint through a PRIVATE bind, so this mount will never reach it. On a
            // CT reboot the clones routinely win that race — measured at 84 ms — and come
            // up showing the template home. Restarting re-binds them to the live overlay.
            Ok(true) => remounted.push(id.to_string()),
            Ok(false) => {}
            Err(e) => tracing::warn!(target: "overlay", "remount: {id}: {e:#}"),
        }
    }
    for id in &remounted {
        match app.docker.is_running(id).await {
            Ok(true) => {
                let restart = async {
                    app.docker.stop_even_if_paused(id).await?;
                    app.docker.start_container(id).await
                };
                match restart.await {
                    Ok(()) => tracing::info!(
                        target: "overlay",
                        "remount: restarted {id} so it binds the live home overlay"
                    ),
                    Err(e) => {
                        tracing::warn!(target: "overlay", "remount: restarting {id}: {e:#}")
                    }
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(target: "overlay", "remount: liveness of {id}: {e:#}"),
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
        assert_eq!(
            read_home_file(&homes, "c1", ".codex/auth.json").unwrap(),
            None
        );
        remove_home_file(&homes, "c1", ".codex/auth.json").unwrap();
        write_home_file(&homes, "c1", ".codex/auth.json", b"{\"a\":1}", 0o600).unwrap();
        assert_eq!(
            read_home_file(&homes, "c1", ".codex/auth.json")
                .unwrap()
                .as_deref(),
            Some(b"{\"a\":1}".as_slice())
        );
        remove_home_file(&homes, "c1", ".codex/auth.json").unwrap();
        assert_eq!(
            read_home_file(&homes, "c1", ".codex/auth.json").unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&homes);
    }

    #[test]
    fn home_write_creates_parents_and_lands_0600() {
        use std::os::unix::fs::PermissionsExt;
        let homes = scratch_homes("parents");
        write_home_file(&homes, "c1", ".pi/agent/auth.json", b"{}", 0o600).unwrap();
        let path = homes.join(".merged").join("c1").join(".pi/agent/auth.json");
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // No temp droppings beside the installed file.
        assert!(std::fs::read_dir(path.parent().unwrap()).unwrap().count() == 1);
        let _ = std::fs::remove_dir_all(&homes);
    }

    #[test]
    fn clearing_browser_locks_takes_the_singletons_and_spares_the_profile() {
        let homes = scratch_homes("browserlocks");
        // The shape a fork inherits: a lock naming the SOURCE clone, a cookie, a socket
        // pointing into a /tmp dir that does not exist in this clone — beside real
        // profile data that must survive.
        write_home_symlink(
            &homes,
            "c1",
            ".config/google-chrome/SingletonLock",
            "src-host-2032",
        )
        .unwrap();
        write_home_symlink(
            &homes,
            "c1",
            ".config/google-chrome/SingletonCookie",
            "7384025234",
        )
        .unwrap();
        write_home_symlink(
            &homes,
            "c1",
            ".config/google-chrome/SingletonSocket",
            "/tmp/com.google.Chrome.wq9GuJ/SingletonSocket",
        )
        .unwrap();
        write_home_file(
            &homes,
            "c1",
            ".config/google-chrome/Preferences",
            b"{}",
            0o600,
        )
        .unwrap();

        clear_browser_profile_locks(&homes, "c1");

        let profile = homes
            .join(MERGED_DIR)
            .join("c1")
            .join(".config/google-chrome");
        for gone in ["SingletonLock", "SingletonCookie", "SingletonSocket"] {
            // symlink_metadata, not exists(): a dangling symlink is exactly what we are
            // removing, and `exists()` follows the link and reports it absent either way.
            assert!(
                std::fs::symlink_metadata(profile.join(gone)).is_err(),
                "{gone} survived"
            );
        }
        assert_eq!(std::fs::read(profile.join("Preferences")).unwrap(), b"{}");

        // A home with no browser profile at all is the common case, not an error.
        clear_browser_profile_locks(&homes, "c1");
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

    /// `~/clones` and `~/shared` become symlinks to the mounts, which now live outside
    /// the home. An entry the clone put there itself is not a symlink and survives
    /// untouched.
    #[test]
    fn home_links_are_created_but_never_replace_a_real_directory() {
        let homes = std::env::temp_dir().join(format!("rmng-links-{}", std::process::id()));
        let home = merged_dir(&homes.to_string_lossy(), "pega-x");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(home.join("shared")).unwrap();
        std::fs::write(home.join("shared/keep-me"), b"x").unwrap();

        ensure_home_links(&home, "pega-x");

        // Nothing was at `clones`, so it is created as a link.
        assert_eq!(
            std::fs::read_link(home.join("clones")).unwrap(),
            Path::new("/clones")
        );
        // `shared` was a directory, so it is still the directory it was.
        assert!(home.join("shared").is_dir());
        assert!(home.join("shared/keep-me").exists());

        // Idempotent, and it creates a link that was never there.
        std::fs::remove_dir_all(home.join("shared")).unwrap();
        ensure_home_links(&home, "pega-x");
        ensure_home_links(&home, "pega-x");
        assert_eq!(
            std::fs::read_link(home.join("clones")).unwrap(),
            Path::new("/clones")
        );
        assert_eq!(
            std::fs::read_link(home.join("shared")).unwrap(),
            Path::new("/shared")
        );

        let _ = std::fs::remove_dir_all(&homes);
    }

    /// The browse root bound at `/clones` is `.merged`, never the homes
    /// parent. Binding the parent showed each clone its siblings' ZFS dataset dirs —
    /// `~/clones/<id>/upper` and `/work` instead of the home — and kept showing the
    /// leftover mountpoint dir of every deleted clone.
    #[test]
    fn the_browse_root_is_the_merged_dir_not_the_homes_parent() {
        let homes = "/srv/rmng-homes";
        assert_eq!(merged_root(homes), Path::new("/srv/rmng-homes/.merged"));
        assert_ne!(merged_root(homes), Path::new(homes));
        // Every clone's home sits directly under it, so the bind shows homes.
        assert_eq!(
            merged_dir(homes, "pega-x"),
            merged_root(homes).join("pega-x")
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
