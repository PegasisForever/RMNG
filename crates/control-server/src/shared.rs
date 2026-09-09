//! The shared pool: `<homes>/.shared`, bound into every clone at `/home/rmng/shared`
//! and served as the `shared` SMB share beside `clones` (see [`crate::smb`]). One flat pool:
//! every clone sees the same bytes and so does an SMB client, so a file dropped anywhere
//! appears everywhere. Read-write from both sides, which is what the root directory's
//! uid-1000 owner buys.
//!
//! The pool lives under the homes parent (not `data/`) because the clone bind is resolved
//! by the Docker daemon on the host: anything inside the server's `data/` volume is
//! container-private and the daemon rejects it with "bind source path does not exist".
//! The homes parent is already a shared bind the daemon sees, so the pool rides it.
//!
//! The bind is part of the container spec ([`crate::docker::CreateSpec::shared_dir`]), so it
//! is present from first boot and survives restarts — no live mount, no re-apply loop. That
//! works because every clone the server sees runs a freshly created container: fresh clones
//! are created with the bind, and the gen-2 migration recreates the rest. (A live mount via
//! `open_tree`/`move_mount` used to cover clones that predated the feature; it died with the
//! last of those.)

use std::path::{Path, PathBuf};

/// The clone user's uid and gid (see [`crate::docker::CLONE_USER`]). The pool's root directory
/// carries this owner so a clone writing through the mount and smbd writing through the
/// `shared` share both land as the same user.
const CLONE_UID: u32 = 1000;

/// The directory holding the shared pool (`<homes>/.shared`). `pub(crate)` so smb.rs
/// single-sources the `shared` share path from it, as it already does for `hosts`.
/// Takes no `data_dir`: the pool deliberately does NOT live under it (see module docs).
pub(crate) fn shared_root() -> PathBuf {
    Path::new(crate::zfs::HOMES_DIR).join(".shared")
}

/// Where the pool appears inside every clone.
pub(crate) fn clone_target() -> String {
    format!("/home/{}/shared", crate::docker::CLONE_USER)
}

/// The pool as an absolute host path for the container bind. Lexical only (no symlink
/// resolution); mirrors the `absolute` helper in smb.rs, which needs the same for smb.conf.
/// The homes parent is an absolute constant, so unlike the old `data/`-relative pool this
/// can never degrade into a daemon-relative bind source.
pub(crate) fn shared_host_dir() -> String {
    let abs = std::path::absolute(shared_root()).unwrap_or_else(|_| shared_root());
    abs.to_string_lossy().into_owned()
}

/// Create the pool (clone-owned) if needed. Runs once at server startup; the create path
/// relies on it, because Docker would otherwise invent a missing bind source as root-owned
/// and break the read-write-both-sides design.
pub fn ensure_pool() {
    let root = shared_root();
    if let Err(e) = std::fs::create_dir_all(&root) {
        tracing::error!(target: "shared", "creating {}: {e}", root.display());
        return;
    }
    // Owned by the clone user, so a clone writing through the mount needs nothing further.
    if let Err(e) = std::os::unix::fs::chown(&root, Some(CLONE_UID), Some(CLONE_UID)) {
        tracing::warn!(target: "shared", "chown {}: {e}", root.display());
    }
    tracing::info!(
        target: "shared",
        "shared pool ready at {} (binds at {} from first boot)",
        root.display(),
        clone_target(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_root_is_under_the_homes_parent() {
        assert_eq!(
            shared_root(),
            Path::new(crate::zfs::HOMES_DIR).join(".shared")
        );
    }

    #[test]
    fn clone_target_is_under_the_clone_users_home() {
        assert_eq!(clone_target(), "/home/rmng/shared");
    }

    #[test]
    fn shared_host_dir_is_absolute() {
        let dir = shared_host_dir();
        assert!(
            Path::new(&dir).is_absolute(),
            "the pool path must never reach the bind relative: {dir}"
        );
        assert!(dir.ends_with(".shared"), "{dir}");
    }
}
