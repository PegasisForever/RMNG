//! The shared pool: `<data_dir>/shared`, bound into every clone at `/home/rmng/shared`
//! and served as the `shared` SMB share beside `clones` (see [`crate::smb`]). One flat pool:
//! every clone sees the same bytes and so does an SMB client, so a file dropped anywhere
//! appears everywhere. Read-write from both sides, which is what the root directory's
//! uid-1000 owner buys.
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

/// The directory holding the shared pool (`<data_dir>/shared`). `pub(crate)` so smb.rs
/// single-sources the `shared` share path from it, as it already does for `hosts`.
pub(crate) fn shared_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("shared")
}

/// Where the pool appears inside every clone.
pub(crate) fn clone_target() -> String {
    format!("/home/{}/shared", crate::docker::CLONE_USER)
}

/// The pool as an absolute host path for the container bind. Lexical only (no symlink
/// resolution); mirrors the `absolute` helper in smb.rs, which needs the same for smb.conf.
/// Docker resolves a relative bind source against the daemon's working directory, so a
/// relative `data_dir` must never reach the spec.
pub(crate) fn shared_host_dir(data_dir: &str) -> String {
    let abs = std::path::absolute(shared_root(data_dir)).unwrap_or_else(|_| shared_root(data_dir));
    abs.to_string_lossy().into_owned()
}

/// Create the pool (clone-owned) if needed. Runs once at server startup; the create path
/// relies on it, because Docker would otherwise invent a missing bind source as root-owned
/// and break the read-write-both-sides design.
pub fn ensure_pool(data_dir: &str) {
    let root = shared_root(data_dir);
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
    fn shared_root_joins_shared() {
        assert_eq!(shared_root("data"), Path::new("data/shared"));
        assert_eq!(
            shared_root("/srv/rmng/data"),
            Path::new("/srv/rmng/data/shared")
        );
    }

    #[test]
    fn clone_target_is_under_the_clone_users_home() {
        assert_eq!(clone_target(), "/home/rmng/shared");
    }

    #[test]
    fn shared_host_dir_is_absolute() {
        let dir = shared_host_dir("data");
        assert!(
            Path::new(&dir).is_absolute(),
            "relative data_dir must never reach the bind: {dir}"
        );
        assert!(dir.ends_with("data/shared"), "{dir}");
    }
}
