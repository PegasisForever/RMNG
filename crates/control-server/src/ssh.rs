//! Bastion `sshd` supervisor + clone SSH key provisioning. Mirrors `crate::smb`: render a
//! pure config, generate/persist host keys, then supervise a foreground `sshd` with capped
//! backoff. A periodic reconciler keeps the bastion's `PermitOpen` allowlist tracking the
//! live fleet and pushes the current `authorized_keys` into each running clone.
//!
//! Topology: the operator runs `ssh -J rmng@<host>:2222 rmng@<clone>`. The bastion (this
//! process's child `sshd`, reusing the uid-1000 `rmng` account) only TCP-forwards to
//! `<clone>:22`; the session terminates end-to-end at the clone's own `sshd`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

use crate::app::App;
use crate::docker::TarEntry;

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

/// The persisted SHARED fleet keypair path. One identity, provisioned into every clone's
/// `~/.ssh`, so any clone can SSH into any sibling with no manual setup — the public half is
/// folded into each clone's `authorized_keys` ([`clone_authorized_keys`]). Stable across
/// control-server restarts (generated once, then reused).
pub fn fleet_key_path(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("ssh/fleet/id_ed25519")
}

/// The shared fleet public-key line, generating the keypair on first call (idempotent).
/// Trimmed to a single line. Used both to append to clones' `authorized_keys` and to seed the
/// push-gate hash so a first-time key generation propagates. Public so the clone reconciler can
/// match a clone's stale `~/.ssh/id_ed25519` against the fleet key during migration.
pub fn fleet_public_key(data_dir: &str) -> Result<String> {
    let key_path = fleet_key_path(data_dir);
    ensure_hostkey(&key_path)?;
    let pub_bytes = std::fs::read(key_path.with_extension("pub"))
        .with_context(|| format!("reading fleet pubkey at {}", key_path.display()))?;
    Ok(String::from_utf8_lossy(&pub_bytes).trim().to_string())
}

/// Operator authorized keys plus the shared fleet public key — the full set every CLONE
/// accepts. The bastion stays operator-only (it never sees the fleet key), so clone↔clone
/// SSH is enabled without widening the operator's door. Order: operator keys first, fleet key
/// last; [`render_authorized_keys`] trims + dedups at render.
pub fn clone_authorized_keys(operator_keys: &[String], fleet_pub: &str) -> Vec<String> {
    let mut keys = operator_keys.to_vec();
    let fleet_pub = fleet_pub.trim();
    if !fleet_pub.is_empty() {
        keys.push(fleet_pub.to_string());
    }
    keys
}

/// The shared fleet client key is provisioned OUTSIDE `~/.ssh` (here) on purpose. GNOME's
/// `gcr-ssh-agent` auto-adopts every private key it finds in `~/.ssh` into the login keyring,
/// and a stale keyring record for that key makes the agent crash during `ssh`'s session-bind —
/// which wedges ALL ssh/git inside the clone (the agent can no longer sign any key). Keeping the
/// fleet key here, referenced from `~/.ssh/config`'s `IdentityFile`, preserves prompt-free
/// `ssh <clone-id>` while the keyring never sees it. Path is tar-root-relative (no leading `/`).
pub(crate) const CLONE_FLEET_KEY_TAR: &str = "home/rmng/.config/rmng/ssh/fleet_ed25519";

/// Dropped at `~rmng/.ssh/config` in every clone so a bare `ssh <clone-id>` from inside a
/// clone is prompt-free: the fleet key (`/home/rmng/.config/rmng/ssh/fleet_ed25519`, kept out
/// of `~/.ssh`) is a default identity, and `accept-new` trusts a sibling's stable host key on
/// first contact (host keys are persisted per clone id).
const CLONE_SSH_CONFIG: &str = "Host *\n    StrictHostKeyChecking accept-new\n    User rmng\n    IdentityFile /home/rmng/.config/rmng/ssh/fleet_ed25519\n";

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

/// The tar entries that provision SSH into one clone:
/// - `~rmng/.ssh/authorized_keys` (operator keys + the shared fleet pubkey; clone-user, 0600),
/// - the shared fleet keypair at [`CLONE_FLEET_KEY_TAR`](.pub) — deliberately OUTSIDE `~/.ssh`
///   (see that const: gcr-ssh-agent crashes on `~/.ssh` keys) — so this clone can dial siblings,
/// - `~rmng/.ssh/config` (`accept-new`, default user, `IdentityFile` → the fleet key) for a
///   prompt-free `ssh <clone-id>`,
/// - the clone's own stable sshd host key + public half (root, 0600 / 0644).
/// Generates+persists the per-clone host key and the shared fleet key on first call. The
/// `~rmng/.ssh` dir is pre-created 700 by the template; the fleet key's `.config/rmng/ssh` dir
/// is created by the reconciler's `ssh_prepare_script` (and auto-created by the docker upload).
pub fn clone_ssh_tar_entries(
    data_dir: &str,
    clone_id: &str,
    keys: &[String],
) -> Result<Vec<TarEntry>> {
    let key_path = clone_hostkey_path(data_dir, clone_id);
    ensure_hostkey(&key_path)?;
    let priv_bytes = std::fs::read(&key_path)
        .with_context(|| format!("reading clone host key {}", key_path.display()))?;
    let pub_bytes = std::fs::read(key_path.with_extension("pub"))
        .with_context(|| format!("reading clone host pubkey for {clone_id}"))?;

    // Shared fleet identity (generated once, persisted): its private half + a small ssh
    // client config go into this clone's ~/.ssh; its public half is folded into the clone's
    // authorized_keys so every clone accepts every other.
    let fleet_path = fleet_key_path(data_dir);
    ensure_hostkey(&fleet_path)?;
    let fleet_priv = std::fs::read(&fleet_path)
        .with_context(|| format!("reading fleet key {}", fleet_path.display()))?;
    let fleet_pub_bytes = std::fs::read(fleet_path.with_extension("pub"))
        .with_context(|| "reading fleet pubkey".to_string())?;
    let fleet_pub = String::from_utf8_lossy(&fleet_pub_bytes).trim().to_string();
    let authorized = clone_authorized_keys(keys, &fleet_pub);

    Ok(vec![
        TarEntry {
            path: "home/rmng/.ssh/authorized_keys".into(),
            data: render_authorized_keys(&authorized).into_bytes(),
            mode: 0o600,
            uid: 1000,
            gid: 1000,
        },
        TarEntry {
            path: CLONE_FLEET_KEY_TAR.into(),
            data: fleet_priv,
            mode: 0o600,
            uid: 1000,
            gid: 1000,
        },
        TarEntry {
            path: format!("{CLONE_FLEET_KEY_TAR}.pub"),
            data: fleet_pub_bytes,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
        },
        TarEntry {
            path: "home/rmng/.ssh/config".into(),
            data: CLONE_SSH_CONFIG.as_bytes().to_vec(),
            mode: 0o600,
            uid: 1000,
            gid: 1000,
        },
        TarEntry {
            path: "etc/ssh/ssh_host_ed25519_key".into(),
            data: priv_bytes,
            mode: 0o600,
            uid: 0,
            gid: 0,
        },
        TarEntry {
            path: "etc/ssh/ssh_host_ed25519_key.pub".into(),
            data: pub_bytes,
            mode: 0o644,
            uid: 0,
            gid: 0,
        },
    ])
}

/// Order-independent hash of a key set (for "did this clone's keys change?" tracking).
pub fn keys_hash(keys: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut sorted: Vec<&str> = keys.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for k in sorted {
        k.hash(&mut h);
    }
    h.finish()
}

/// Restart backoff: first retry after `BASE_BACKOFF`, doubling per consecutive quick crash
/// up to `MAX_BACKOFF`. A run that stays up past `STABLE_RUN` resets the counter (mirrors
/// `smb::backoff`). Pure + saturating throughout, so a runaway crash loop can never
/// overflow the multiply — it just pins at `MAX_BACKOFF`.
const BASE_BACKOFF: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const STABLE_RUN: Duration = Duration::from_secs(60);
/// How often to re-render the bastion allowlist and push keys into clones.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);

pub fn backoff(failures: u32) -> Duration {
    BASE_BACKOFF.saturating_mul(2u32.saturating_pow(failures)).min(MAX_BACKOFF)
}

/// Sorted, deduped ids of active managed clones — the `PermitOpen` allowlist source.
pub fn managed_clone_ids(hosts: &[wire::Host]) -> Vec<String> {
    let mut ids: Vec<String> = hosts
        .iter()
        .filter(|h| h.managed && !h.archived)
        .map(|h| h.id.clone())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Write the bastion `authorized_keys` + `sshd_config` from current state. Returns whether
/// the `sshd_config` content changed (⇒ caller should reload). Best-effort file writes.
fn render_bastion_files(app: &App, data_dir: &str) -> bool {
    let cfg = app.config();
    let keys = render_authorized_keys(&cfg.ssh.authorized_keys);
    let _ = std::fs::create_dir_all("/etc/rmng/ssh");
    let _ = std::fs::write(BASTION_AUTHORIZED_KEYS, keys);

    let ids = managed_clone_ids(&app.store.get().hosts);
    // Absolute HostKey path: on SIGHUP sshd re-execs and re-resolves relative paths from
    // its CWD, so a relative HostKey makes a reload die with "no hostkeys available --
    // exiting" (found in live E2E). `std::path::absolute` is lexical (no I/O), mirroring
    // smb.rs's `absolute_hosts_root`.
    let key_path = std::path::absolute(bastion_hostkey_path(data_dir))
        .unwrap_or_else(|_| bastion_hostkey_path(data_dir));
    let want = render_bastion_sshd_config(
        cfg.listen.bastion,
        key_path.to_str().unwrap_or_default(),
        BASTION_AUTHORIZED_KEYS,
        &ids,
    );
    let changed = std::fs::read_to_string(BASTION_SSHD_CONFIG).ok().as_deref() != Some(&want);
    if changed {
        let _ = std::fs::write(BASTION_SSHD_CONFIG, &want);
    }
    changed
}

/// Push the current `authorized_keys` into each running managed clone whose last-pushed
/// hash differs. `pushed` tracks `clone_id → keys_hash` so a tar upload happens only on a
/// real change or a newly-seen clone. Best-effort per clone.
async fn push_keys_to_clones(app: &App, data_dir: &str, pushed: &mut HashMap<String, u64>) {
    let cfg = app.config();
    // Gate on the EFFECTIVE clone key set (operator keys + fleet pubkey) so a first-time
    // fleet-key generation triggers a push even when the operator keys are unchanged.
    let fleet_pub = fleet_public_key(data_dir).unwrap_or_default();
    let hash = keys_hash(&clone_authorized_keys(&cfg.ssh.authorized_keys, &fleet_pub));
    for host in app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived)
    {
        if pushed.get(&host.id) == Some(&hash) {
            continue;
        }
        if !app.docker.is_running(&host.id).await.unwrap_or(false) {
            continue; // stopped clones get keys at next provision/boot
        }
        match clone_ssh_tar_entries(data_dir, &host.id, &cfg.ssh.authorized_keys) {
            // Push the ~/.ssh material live (authorized_keys + shared fleet key + config);
            // the clone's own host key is provision-time only (changing it under a running
            // sshd would need a restart), so filter to the home/ files.
            Ok(entries) => {
                let ak: Vec<_> =
                    entries.into_iter().filter(|e| e.path.starts_with("home/")).collect();
                match app.docker.upload_tar(&host.id, ak).await {
                    Ok(()) => {
                        pushed.insert(host.id.clone(), hash);
                        tracing::info!(target: "ssh", "pushed authorized_keys to {}", host.id);
                    }
                    Err(e) => tracing::warn!(target: "ssh", "push keys to {} failed: {e}", host.id),
                }
            }
            Err(e) => tracing::warn!(target: "ssh", "key material for {} failed: {e}", host.id),
        }
    }
}

/// One-shot apply used by the config PUT path so a key add/remove propagates immediately
/// rather than waiting for the reconcile tick. Rewrites the bastion `authorized_keys`
/// (read fresh by sshd per connection — no reload needed) and pushes to running clones.
pub async fn apply_now(app: &App) {
    let data_dir = app.config().data_dir.clone();
    render_bastion_files(app, &data_dir);
    let mut once = std::collections::HashMap::new();
    push_keys_to_clones(app, &data_dir, &mut once).await;
}

fn spawn_sshd() -> std::io::Result<Child> {
    // `-D` foreground, `-e` log to stderr so the supervisor's pipe sees failures.
    Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f", BASTION_SSHD_CONFIG])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

async fn log_lines<R: AsyncRead + Unpin>(reader: R) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => tracing::info!(target: "ssh", "{line}"),
            Ok(None) => break,
            Err(_) => continue,
        }
    }
}

/// Run one `sshd` to exit while concurrently (a) draining its logs and (b) reconciling the
/// allowlist + clone keys every `RECONCILE_INTERVAL`, reloading `sshd` (SIGHUP) when the
/// config changed. Returns when `sshd` exits.
async fn run_sshd(mut child: Child, app: &App, data_dir: &str, pushed: &mut HashMap<String, u64>) {
    let out = child.stdout.take();
    let err = child.stderr.take();
    let pid = child.id();
    let logs = async {
        tokio::join!(
            async { if let Some(r) = out { log_lines(r).await } },
            async { if let Some(r) = err { log_lines(r).await } },
        );
    };
    let reconcile = async {
        loop {
            tokio::time::sleep(RECONCILE_INTERVAL).await;
            let changed = render_bastion_files(app, data_dir);
            if changed {
                if let Some(pid) = pid {
                    // Reload PermitOpen without dropping live tunnels.
                    let _ = Command::new("kill").args(["-HUP", &pid.to_string()]).status().await;
                    tracing::info!(target: "ssh", "reloaded bastion sshd (fleet changed)");
                }
            }
            push_keys_to_clones(app, data_dir, pushed).await;
        }
    };
    tokio::select! {
        status = child.wait() => match status {
            Ok(s) => tracing::warn!(target: "ssh", "bastion sshd exited ({s}) — restarting"),
            Err(e) => tracing::warn!(target: "ssh", "waiting on bastion sshd failed: {e}"),
        },
        _ = logs => {}
        _ = reconcile => {}
    }
}

/// Ensure the bastion login account (`rmng`, reused from the SMB share) can authenticate
/// by public key. `useradd`/`smbpasswd` leave its Unix password field `!` (locked), and
/// the bastion runs `sshd` with `UsePAM no`, which refuses a locked account outright
/// ("User rmng not allowed because account is locked" — found in live E2E) even for pubkey
/// auth. Setting the field to `*` (disabled, not locked) lets sshd accept it for pubkey
/// while still permitting no password login. Idempotent + best-effort: a dev box without
/// the account or without root just no-ops (its child output is discarded).
async fn ensure_bastion_account() {
    let _ = Command::new("usermod")
        .args(["-p", "*", "rmng"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// Ensure the host key + login account, render the initial config, then supervise `sshd`
/// forever with capped backoff. Never returns. Empty keys are fine — the bastion just runs
/// with an empty `authorized_keys` (no one can auth) and `PermitOpen none`.
pub async fn run(app: App) {
    let data_dir = app.config().data_dir.clone();
    if let Err(e) = ensure_hostkey(&bastion_hostkey_path(&data_dir)) {
        tracing::error!(target: "ssh", "bastion host key generation failed: {e}");
    }
    ensure_bastion_account().await;
    let mut pushed: HashMap<String, u64> = HashMap::new();
    let mut failures: u32 = 0;
    let mut spawn_error_logged = false;
    loop {
        render_bastion_files(&app, &data_dir);
        push_keys_to_clones(&app, &data_dir, &mut pushed).await;
        let started = Instant::now();
        match spawn_sshd() {
            Ok(child) => {
                spawn_error_logged = false;
                tracing::info!(target: "ssh", "bastion sshd listening on :{}", app.config().listen.bastion);
                run_sshd(child, &app, &data_dir, &mut pushed).await;
            }
            Err(e) if !spawn_error_logged => {
                tracing::error!(target: "ssh", "failed to spawn sshd (openssh-server installed?): {e}");
                spawn_error_logged = true;
            }
            Err(e) => tracing::debug!(target: "ssh", "sshd spawn still failing: {e}"),
        }
        if started.elapsed() >= STABLE_RUN {
            failures = 0;
        }
        let delay = backoff(failures);
        failures = failures.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
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

    #[test]
    fn clone_tar_entries_have_correct_paths_modes_owners() {
        if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
            eprintln!("skipping: ssh-keygen not installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("rmng-ssh-tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entries =
            clone_ssh_tar_entries(dir.to_str().unwrap(), "clone-a", &["ssh-ed25519 AAAA a".into()]).unwrap();

        // authorized_keys = operator key + the generated fleet pubkey.
        let ak = entries.iter().find(|e| e.path == "home/rmng/.ssh/authorized_keys").expect("authorized_keys entry");
        assert_eq!(ak.mode, 0o600);
        assert_eq!((ak.uid, ak.gid), (1000, 1000));
        let fleet_pub =
            std::fs::read_to_string(fleet_key_path(dir.to_str().unwrap()).with_extension("pub")).unwrap();
        let fleet_pub = fleet_pub.trim();
        let ak_text = String::from_utf8(ak.data.clone()).unwrap();
        assert!(ak_text.contains("ssh-ed25519 AAAA a"), "operator key present:\n{ak_text}");
        assert!(ak_text.contains(fleet_pub), "fleet pubkey folded in:\n{ak_text}");

        // Shared fleet keypair lives OUTSIDE ~/.ssh (gcr-ssh-agent crash avoidance), clone-user owned.
        let id = entries
            .iter()
            .find(|e| e.path == "home/rmng/.config/rmng/ssh/fleet_ed25519")
            .expect("fleet private key entry");
        assert_eq!(id.mode, 0o600);
        assert_eq!((id.uid, id.gid), (1000, 1000));
        assert!(!id.data.is_empty(), "fleet private key material present");
        assert!(entries.iter().any(|e| e.path == "home/rmng/.config/rmng/ssh/fleet_ed25519.pub"
            && e.mode == 0o644
            && (e.uid, e.gid) == (1000, 1000)));
        // No private key must land in ~/.ssh (that is exactly what poisons gcr-ssh-agent).
        assert!(
            !entries.iter().any(|e| e.path == "home/rmng/.ssh/id_ed25519"),
            "fleet key must NOT be written into ~/.ssh"
        );
        let cfg = entries.iter().find(|e| e.path == "home/rmng/.ssh/config").expect("ssh config entry");
        assert_eq!(cfg.mode, 0o600);
        assert_eq!((cfg.uid, cfg.gid), (1000, 1000));
        let cfg_text = String::from_utf8(cfg.data.clone()).unwrap();
        assert!(cfg_text.contains("StrictHostKeyChecking accept-new"));
        assert!(
            cfg_text.contains("IdentityFile /home/rmng/.config/rmng/ssh/fleet_ed25519"),
            "ssh config points IdentityFile at the relocated fleet key:\n{cfg_text}"
        );

        // The clone's own sshd host key is still root-owned.
        let hk = entries.iter().find(|e| e.path == "etc/ssh/ssh_host_ed25519_key").expect("host key entry");
        assert_eq!(hk.mode, 0o600);
        assert_eq!((hk.uid, hk.gid), (0, 0));
        assert!(entries.iter().any(|e| e.path == "etc/ssh/ssh_host_ed25519_key.pub" && e.mode == 0o644));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clone_authorized_keys_appends_fleet_pubkey() {
        let out = clone_authorized_keys(&["ssh-ed25519 OP op".into()], "  ssh-ed25519 FLEET fleet  ");
        assert_eq!(out, vec!["ssh-ed25519 OP op".to_string(), "ssh-ed25519 FLEET fleet".to_string()]);
        // No operator keys ⇒ just the fleet key; empty fleet ⇒ operator keys only.
        assert_eq!(clone_authorized_keys(&[], "ssh-ed25519 FLEET fleet"), vec!["ssh-ed25519 FLEET fleet".to_string()]);
        assert_eq!(clone_authorized_keys(&["ssh-ed25519 OP op".into()], "  "), vec!["ssh-ed25519 OP op".to_string()]);
    }

    #[test]
    fn fleet_key_path_is_under_data_dir() {
        assert_eq!(fleet_key_path("/data").to_str().unwrap(), "/data/ssh/fleet/id_ed25519");
    }

    #[test]
    fn keys_hash_is_order_independent_and_change_sensitive() {
        let a = keys_hash(&["k1".into(), "k2".into()]);
        let b = keys_hash(&["k2".into(), "k1".into()]);
        assert_eq!(a, b, "order must not matter");
        let c = keys_hash(&["k1".into()]);
        assert_ne!(a, c, "a changed key set must change the hash");
    }

    #[test]
    fn backoff_escalates_then_caps() {
        assert_eq!(backoff(0), std::time::Duration::from_secs(15));
        assert_eq!(backoff(4).as_secs(), 240);
        assert_eq!(backoff(10).as_secs(), 300); // capped
        assert_eq!(backoff(u32::MAX).as_secs(), 300); // saturating
    }

    #[test]
    fn changing_keys_changes_rendered_authorized_keys() {
        assert_ne!(
            render_authorized_keys(&["ssh-ed25519 A a".into()]),
            render_authorized_keys(&["ssh-ed25519 B b".into()])
        );
    }

    #[test]
    fn managed_clone_ids_filters_and_sorts() {
        let mut h1 = wire::Host::default();
        h1.id = "b-clone".into();
        h1.managed = true;
        let mut h2 = wire::Host::default();
        h2.id = "a-clone".into();
        h2.managed = true;
        let mut unmanaged = wire::Host::default();
        unmanaged.id = "legacy".into();
        unmanaged.managed = false;
        let mut archived = wire::Host::default();
        archived.id = "retained".into();
        archived.managed = true;
        archived.archived = true;
        let ids = managed_clone_ids(&[h1, unmanaged, h2, archived]);
        assert_eq!(ids, vec!["a-clone".to_string(), "b-clone".to_string()]);
    }
}
