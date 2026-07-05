//! Bastion `sshd` supervisor + clone SSH key provisioning. Mirrors `crate::smb`: render a
//! pure config, generate/persist host keys, then supervise a foreground `sshd` with capped
//! backoff. A periodic reconciler keeps the bastion's `PermitOpen` allowlist tracking the
//! live fleet and pushes the current `authorized_keys` into each running clone.
//!
//! Topology: the operator runs `ssh -J rmng@<host>:2222 rmng@<clone>`. The bastion (this
//! process's child `sshd`, reusing the uid-1000 `rmng` account) only TCP-forwards to
//! `<clone>:22`; the session terminates end-to-end at the clone's own `sshd`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};

/// Absolute path the bastion `sshd` reads authorized keys from. Outside any home (the
/// `rmng` account has none), root-owned — so `StrictModes` is satisfied.
pub const BASTION_AUTHORIZED_KEYS: &str = "/etc/rmng/ssh/authorized_keys";
/// Where we render the bastion `sshd_config`.
pub const BASTION_SSHD_CONFIG: &str = "/etc/rmng/ssh/sshd_config";

/// One authorized-keys line per public key: trimmed, blanks dropped, de-duplicated
/// (first-seen order), trailing newline. Empty input ⇒ empty string (an empty file is a
/// valid "no keys" state). Pure — unit-tested.
pub fn render_authorized_keys(keys: &[String]) -> String {
    let mut seen = HashSet::new();
    let mut out = String::new();
    for k in keys {
        let k = k.trim();
        if k.is_empty() || !seen.insert(k) {
            continue;
        }
        out.push_str(k);
        out.push('\n');
    }
    out
}

/// The jump-only bastion `sshd_config`. `clone_ids` become the `PermitOpen` allowlist
/// (matched LITERALLY against the client's requested `<host>:22`, which is the clone id).
/// Empty fleet ⇒ `PermitOpen none`. Pure — unit-tested.
pub fn render_bastion_sshd_config(
    bastion_port: u16,
    host_key_path: &str,
    authorized_keys_path: &str,
    clone_ids: &[String],
) -> String {
    let permit_open = if clone_ids.is_empty() {
        "none".to_string()
    } else {
        clone_ids.iter().map(|id| format!("{id}:22")).collect::<Vec<_>>().join(" ")
    };
    format!(
        "# Rendered by rmng control-server — do not edit.
Port {bastion_port}
AddressFamily any
ListenAddress 0.0.0.0
HostKey {host_key_path}
PidFile none
LogLevel INFO

PasswordAuthentication no
PubkeyAuthentication yes
KbdInteractiveAuthentication no
PermitRootLogin no
UsePAM no
AllowUsers rmng
AuthorizedKeysFile {authorized_keys_path}

# Jump-only: forwarding to live clones' :22 is the ONLY thing this bastion permits.
AllowTcpForwarding yes
AllowAgentForwarding no
X11Forwarding no
PermitTTY no
GatewayPorts no
PermitTunnel no

Match User rmng
    PermitOpen {permit_open}
    ForceCommand /usr/sbin/nologin
"
    )
}

/// The persisted bastion host key path (stable across control-server restarts).
pub fn bastion_hostkey_path(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("ssh/bastion/ssh_host_ed25519_key")
}

/// The persisted per-clone host key path (stable across clone rebuilds of the same id).
pub fn clone_hostkey_path(data_dir: &str, clone_id: &str) -> PathBuf {
    Path::new(data_dir).join("ssh/clones").join(clone_id).join("ssh_host_ed25519_key")
}

/// Generate an ed25519 host key at `key_path` (+ `.pub`) if it doesn't already exist.
/// Idempotent: an existing key is left byte-for-byte untouched (so identity is stable).
/// Parent dirs are created 0700.
pub fn ensure_hostkey(key_path: &Path) -> Result<()> {
    if key_path.exists() {
        return Ok(());
    }
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        // 0700 so private keys aren't world/group-readable (sshd StrictModes).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod 0700 {}", parent.display()))?;
        }
    }
    let status = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-C", "rmng", "-f"])
        .arg(key_path)
        .status()
        .context("running ssh-keygen (is openssh installed?)")?;
    anyhow::ensure!(status.success(), "ssh-keygen failed for {}", key_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_keys_dedup_and_newline() {
        let out = render_authorized_keys(&[
            "ssh-ed25519 AAAA a".into(),
            "  ".into(),               // blank → dropped
            "ssh-ed25519 AAAA a".into(), // dup → dropped
            "ssh-ed25519 BBBB b".into(),
        ]);
        assert_eq!(out, "ssh-ed25519 AAAA a\nssh-ed25519 BBBB b\n");
    }

    #[test]
    fn authorized_keys_empty_is_empty_string() {
        assert_eq!(render_authorized_keys(&[]), "");
    }

    #[test]
    fn bastion_config_has_load_bearing_directives() {
        let out = render_bastion_sshd_config(
            2222,
            "/data/ssh/bastion/ssh_host_ed25519_key",
            BASTION_AUTHORIZED_KEYS,
            &["clone-a".into(), "clone-b".into()],
        );
        for needle in [
            "Port 2222",
            "HostKey /data/ssh/bastion/ssh_host_ed25519_key",
            "PasswordAuthentication no",
            "PermitRootLogin no",
            "AllowUsers rmng",
            "AllowTcpForwarding yes",
            "AuthorizedKeysFile /etc/rmng/ssh/authorized_keys",
            "Match User rmng",
            "PermitOpen clone-a:22 clone-b:22",
        ] {
            assert!(out.contains(needle), "bastion sshd_config missing `{needle}`:\n{out}");
        }
    }

    #[test]
    fn bastion_config_empty_fleet_denies_all_forwards() {
        // No clones ⇒ PermitOpen none (a valid key can auth but can open nothing).
        let out = render_bastion_sshd_config(2222, "/k", BASTION_AUTHORIZED_KEYS, &[]);
        assert!(out.contains("PermitOpen none"), "empty fleet must deny forwards:\n{out}");
    }

    #[test]
    fn bastion_permitopen_uses_literal_ids() {
        // PermitOpen is matched literally against the client's requested host:port, which
        // is the clone id — so entries must be exactly `<id>:22`, no resolution.
        let out = render_bastion_sshd_config(2222, "/k", BASTION_AUTHORIZED_KEYS, &["w-cp-claude".into()]);
        assert!(out.contains("PermitOpen w-cp-claude:22"), "{out}");
    }

    #[test]
    fn hostkey_paths_are_under_data_dir() {
        assert_eq!(
            bastion_hostkey_path("/data").to_str().unwrap(),
            "/data/ssh/bastion/ssh_host_ed25519_key"
        );
        assert_eq!(
            clone_hostkey_path("/data", "clone-a").to_str().unwrap(),
            "/data/ssh/clones/clone-a/ssh_host_ed25519_key"
        );
    }

    #[test]
    fn ensure_hostkey_generates_once_and_is_idempotent() {
        // Needs openssh-client for ssh-keygen; skip cleanly where it's absent (minimal CI).
        if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
            eprintln!("skipping: ssh-keygen not installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("rmng-ssh-test-{}", std::process::id()));
        let key = dir.join("ssh_host_ed25519_key");
        let _ = std::fs::remove_dir_all(&dir);

        ensure_hostkey(&key).unwrap();
        assert!(key.exists(), "private key created");
        assert!(key.with_extension("pub").exists(), "public key created");
        let first = std::fs::read(&key).unwrap();

        ensure_hostkey(&key).unwrap(); // idempotent: must NOT regenerate
        assert_eq!(std::fs::read(&key).unwrap(), first, "second call regenerated the key");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
