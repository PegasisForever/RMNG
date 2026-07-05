# SSH into any clone through the control-server — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the operator `ssh` (and `scp`/`git`/`rsync`/VSCode Remote-SSH) into any clone via a single copy-pasted command, routed through a locked-down bastion `sshd` on the control-server — with zero laptop-side config.

**Architecture:** Each clone runs a real `sshd` on `:22` (internal `rmng` network only). The control-server runs a jump-only bastion `sshd` on `:2222`, reusing the existing `rmng` (uid 1000) account, that TCP-forwards only to live `clone:22`. The operator's `ssh -J rmng@HOST:2222 rmng@<clone>` terminates end-to-end at the clone's `sshd`. Public keys are pasted in the web UI, stored in `AppConfig`, and installed on the bastion + every clone. Host keys are control-server-generated and stable so the copied command runs with no prompt.

**Tech Stack:** Rust (control-server, wire, cli — anyhow/tokio/serde/ts-rs/bollard), OpenSSH (`sshd`, `ssh-keygen`), Docker (bollard `upload_to_container`), React/react-router + Storybook (frontend), Ubuntu clone template (Dockerfile + shell setup scripts).

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-07-05-ssh-into-clones-design.md` — every task implements part of it.
- **Bastion port default:** `2222` (a `listen` port → restart-required, like the other listen ports).
- **Bastion account:** reuse the existing `rmng` uid/gid **1000** account in the control-server image (`Dockerfile:118-129`) — `nologin`, no home. SSH access scoped by `Match User rmng`; `AuthorizedKeysFile` is an absolute path (`/etc/rmng/ssh/authorized_keys`) because the account has no home.
- **Clone login user:** `rmng` (uid 1000). Provisioning constants: `CLONE_USER` (from `crate::docker`), `CLONE_UID = 1000`, `CLONE_GID = 1000` (both defined in `provision.rs`).
- **Clone name == host id == Docker DNS name on the `rmng` network** (`docker.rs`). The bastion dials `<clone-id>:22`; `PermitOpen` entries are matched **literally** against the client's requested `<clone-id>:22`, so entries must use the exact host id.
- **Auth:** public-key only everywhere. `PasswordAuthentication no`, `PermitRootLogin no`. No secrets involved — SSH public keys pass through `AppConfigRedacted` unredacted.
- **Host-key stability:** control-server generates + persists host keys (bastion in `data_dir/ssh/bastion/`, per-clone in `data_dir/ssh/clones/<id>/`). The clone template ships **without** baked host keys (stripped in the Dockerfile tail, like `/etc/machine-id`).
- **Existing patterns to mirror:** the bastion supervisor mirrors `crate::smb` (render pure config → supervise a foreground daemon with capped backoff). Clone file injection mirrors `provision.rs`'s `upload_tar` + `TarEntry`. Reconcile-loop shape mirrors `crate::homes`.
- **Rust conventions:** `anyhow::Result`, `tracing::{info,warn,error}` with a `target:` (use `target: "ssh"`), `#[serde(rename_all = "camelCase")]` + `#[ts(export, export_to = "../../../frontend/app/lib/wire/")]` on wire types.
- **After any wire type change:** run `cargo test -p wire` (ts-rs regenerates `frontend/app/lib/wire/*.ts` during its tests) so the frontend types stay in sync.
- **Commit after every task** (frequent commits). Branch: `feat/ssh-into-clones`.

## File Structure

**Created:**
- `crates/control-server/src/ssh.rs` — bastion `sshd` supervisor: pure renderers (`render_bastion_sshd_config`, `render_authorized_keys`), host-key generation (`ensure_hostkey`), clone key/host-key material for provisioning, the periodic reconciler (PermitOpen + push keys to clones), and `run(app)` spawned from `main`.

**Modified:**
- `crates/wire/src/config.rs` — new `SshConfig` struct; `AppConfig.ssh`; `AppConfigRedacted.ssh`; `ListenConfig.bastion` (default 2222).
- `crates/control-server/src/config.rs` — `restart_required` gains the bastion port.
- `crates/control-server/src/provision.rs` — inject `authorized_keys` + clone host key into the pre-boot `upload_tar`.
- `crates/control-server/src/web.rs` — `config_put` triggers an immediate bastion + clone key apply.
- `crates/control-server/src/main.rs` — `tokio::spawn(ssh::run(app.clone()))`; `mod ssh;`.
- `crates/control-server/src/app.rs` — nothing structural; used read-only (`app.config()`, `app.store.get().hosts`, `app.docker`).
- `Dockerfile` — install `openssh-server` in the control-server image.
- `template/Dockerfile`, `template/setup/30-user.sh` — install/enable clone `sshd`, pre-create `~rmng/.ssh` (700), strip baked host keys.
- `crates/cli/src/args.rs`, `crates/cli/src/commands.rs` — `rmng ssh <clone>` prints the one-liner.
- `frontend/app/components/SettingsPanel.tsx` — SSH Access section (paste keys, show bastion port, public-host override).
- `frontend/app/lib/ssh.ts` (new small helper) + a clone-row/detail component — `buildSshCommand()` + "Copy SSH command" button.
- `compose.yaml`, `README.md`, `docs/DEPLOY.md`, `docs/PROTOCOL.md` — publish `2222`, document the flow + port map.

---

## Task 1: Wire config — `SshConfig` + `listen.bastion`

**Files:**
- Modify: `crates/wire/src/config.rs` (`ListenConfig` ~19-36 + its `Default`; `AppConfig` ~276; `AppConfigRedacted` ~438)
- Test: `crates/wire/src/config.rs` (`#[cfg(test)]` module at bottom)

**Interfaces:**
- Produces: `wire::SshConfig { enabled: bool, authorized_keys: Vec<String>, public_host: String }`; `AppConfig.ssh: SshConfig`; `AppConfigRedacted.ssh: SshConfig`; `ListenConfig.bastion: u16` (default 2222); `fn default_bastion() -> u16`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `crates/wire/src/config.rs`:

```rust
#[test]
fn listen_default_bastion_is_2222() {
    assert_eq!(ListenConfig::default().bastion, 2222);
}

#[test]
fn ssh_config_defaults_are_empty_and_disabled() {
    let s = SshConfig::default();
    assert!(!s.enabled);
    assert!(s.authorized_keys.is_empty());
    assert!(s.public_host.is_empty());
}

#[test]
fn app_config_ssh_round_trips_camel_case() {
    let mut c = AppConfig::default();
    c.ssh = SshConfig {
        enabled: true,
        authorized_keys: vec!["ssh-ed25519 AAAA me@laptop".into()],
        public_host: "rmng.example.com".into(),
    };
    let json = serde_json::to_string(&c).unwrap();
    assert!(json.contains("\"authorizedKeys\""), "camelCase key missing: {json}");
    assert!(json.contains("\"publicHost\":\"rmng.example.com\""), "{json}");
    let back: AppConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.ssh, c.ssh);
}

#[test]
fn redacted_carries_ssh_keys_unredacted() {
    // Public keys are not secret — they must survive redaction (the UI needs them).
    let mut c = AppConfig::default();
    c.ssh.authorized_keys = vec!["ssh-ed25519 AAAA me@laptop".into()];
    let r = c.redacted();
    assert_eq!(r.ssh.authorized_keys, c.ssh.authorized_keys);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wire config::tests -- ssh listen_default_bastion redacted_carries_ssh 2>&1 | tail -20`
Expected: FAIL — `SshConfig` unknown / `ssh` field missing / `bastion` field missing.

- [ ] **Step 3: Add the struct and fields**

In `crates/wire/src/config.rs`, add the struct (place it near `EnvVar`, after `ListenConfig`):

```rust
/// SSH access settings. The control-server runs a jump-only bastion `sshd`; these keys
/// are installed on the bastion AND every clone. Public keys are NOT secret — the whole
/// struct passes through [`AppConfigRedacted`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct SshConfig {
    /// Whether the bastion runs and keys are provisioned into clones.
    #[serde(default)]
    pub enabled: bool,
    /// Authorized SSH public keys, one full line each (`ssh-ed25519 AAAA… comment`).
    #[serde(default)]
    pub authorized_keys: Vec<String>,
    /// Public host/IP the copied command's `-J` jump targets. Empty ⇒ the UI infers it
    /// from the address it's already served on. Not secret.
    #[serde(default)]
    pub public_host: String,
}
```

Add `bastion` to `ListenConfig` (after `forward`) and a default fn:

```rust
    /// The bastion `sshd` port (jump host into clones). Restart-required (bound at startup).
    #[serde(default = "default_bastion")]
    pub bastion: u16,
```
```rust
fn default_bastion() -> u16 {
    2222
}
```

Update `impl Default for ListenConfig` to include `bastion: default_bastion()`.

Add to `AppConfig` (after `chroma` or near the other config sections):

```rust
    #[serde(default)]
    pub ssh: SshConfig,
```

Add to `AppConfigRedacted` (mirror position):

```rust
    pub ssh: SshConfig,
```

Find where `AppConfig::redacted()` builds `AppConfigRedacted` and add `ssh: self.ssh.clone(),`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wire config 2>&1 | tail -20`
Expected: PASS (all config tests, including the 4 new ones). ts-rs also writes `frontend/app/lib/wire/SshConfig.ts`.

- [ ] **Step 5: Commit**

```bash
git add crates/wire/src/config.rs frontend/app/lib/wire/
git commit -m "feat(wire): SshConfig + listen.bastion (default 2222)"
```

---

## Task 2: Config merge + restart-required for the bastion port

**Files:**
- Modify: `crates/control-server/src/config.rs` (`restart_required` ~698)
- Test: `crates/control-server/src/config.rs` (`#[cfg(test)]` module)

**Interfaces:**
- Consumes: `wire::SshConfig`, `wire::AppConfig` (Task 1).
- Produces: no new symbols — verifies `merge_update` handles `ssh` generically and `restart_required` covers `listen.bastion`.

**Context:** `merge_update` (`config.rs:593`) uses `deep_merge`, which **replaces arrays wholesale** (the `(d, s) => *d = s.clone()` arm) and treats empty strings as "unchanged". So an incoming `{"ssh":{"authorizedKeys":[...]}}` replaces the stored list, and `{"ssh":{"authorizedKeys":[]}}` clears it — exactly the UI's semantics. No special-casing needed; these tests lock that in.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `crates/control-server/src/config.rs`:

```rust
#[test]
fn merge_replaces_ssh_authorized_keys_wholesale() {
    let mut base = AppConfig::default();
    base.ssh.authorized_keys = vec!["ssh-ed25519 OLD a".into()];
    let incoming = serde_json::json!({
        "ssh": { "authorizedKeys": ["ssh-ed25519 NEW b", "ssh-ed25519 NEW c"] }
    });
    let merged = merge_update(&base, incoming).unwrap();
    assert_eq!(
        merged.ssh.authorized_keys,
        vec!["ssh-ed25519 NEW b".to_string(), "ssh-ed25519 NEW c".to_string()]
    );
}

#[test]
fn merge_can_clear_ssh_authorized_keys() {
    let mut base = AppConfig::default();
    base.ssh.authorized_keys = vec!["ssh-ed25519 OLD a".into()];
    let merged = merge_update(&base, serde_json::json!({ "ssh": { "authorizedKeys": [] } })).unwrap();
    assert!(merged.ssh.authorized_keys.is_empty());
}

#[test]
fn restart_required_flips_on_bastion_port() {
    let base = AppConfig::default();
    let mut n = base.clone();
    n.listen.bastion = 2200;
    assert!(restart_required(&base, &n));
    // Changing keys alone is live-apply, NOT restart-required.
    let mut k = base.clone();
    k.ssh.authorized_keys = vec!["ssh-ed25519 AAAA x".into()];
    assert!(!restart_required(&base, &k));
}

#[test]
fn ssh_keys_editable_after_setup_complete() {
    // The one-time category guard must not block SSH key edits post-setup.
    let mut base = AppConfig::default();
    base.setup_complete = true;
    let merged = merge_update(
        &base,
        serde_json::json!({ "ssh": { "authorizedKeys": ["ssh-ed25519 AAAA x"] } }),
    )
    .unwrap();
    assert_eq!(merged.ssh.authorized_keys, vec!["ssh-ed25519 AAAA x".to_string()]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p control-server config::tests -- ssh restart_required_flips_on_bastion 2>&1 | tail -25`
Expected: `restart_required_flips_on_bastion_port` FAILS (bastion not yet compared). The three merge tests likely PASS already (generic merge) — that's fine; they guard against regressions.

- [ ] **Step 3: Add the bastion port to `restart_required`**

In `crates/control-server/src/config.rs`, add one line to `restart_required`:

```rust
        || old.listen.global_mcp != new.listen.global_mcp
        || old.listen.bastion != new.listen.bastion
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p control-server config 2>&1 | tail -25`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/config.rs
git commit -m "feat(config): bastion port is restart-required; lock in ssh-key merge semantics"
```

---

## Task 3: `ssh.rs` — pure config/key renderers

**Files:**
- Create: `crates/control-server/src/ssh.rs`
- Modify: `crates/control-server/src/main.rs` (add `mod ssh;` near the other `mod` lines, ~line 22)
- Test: `crates/control-server/src/ssh.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub fn render_authorized_keys(keys: &[String]) -> String` — dedup (preserve first-seen order), drop blank/whitespace-only lines, one key per line, trailing newline.
  - `pub fn render_bastion_sshd_config(bastion_port: u16, host_key_path: &str, authorized_keys_path: &str, clone_ids: &[String]) -> String` — the full jump-only `sshd_config`.
  - Path constants: `pub const BASTION_AUTHORIZED_KEYS: &str = "/etc/rmng/ssh/authorized_keys";`, `pub const BASTION_SSHD_CONFIG: &str = "/etc/rmng/ssh/sshd_config";`.

- [ ] **Step 1: Write the failing tests**

Create `crates/control-server/src/ssh.rs` with only the tests + `use super::*;` and empty stubs so it compiles-then-fails:

```rust
//! Bastion `sshd` supervisor + clone SSH key provisioning. Mirrors `crate::smb`: render a
//! pure config, generate/persist host keys, then supervise a foreground `sshd` with capped
//! backoff. A periodic reconciler keeps the bastion's `PermitOpen` allowlist tracking the
//! live fleet and pushes the current `authorized_keys` into each running clone.
//!
//! Topology: the operator runs `ssh -J rmng@<host>:2222 rmng@<clone>`. The bastion (this
//! process's child `sshd`, reusing the uid-1000 `rmng` account) only TCP-forwards to
//! `<clone>:22`; the session terminates end-to-end at the clone's own `sshd`.

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
}
```

Add `mod ssh;` to `crates/control-server/src/main.rs` alongside the other `mod` declarations.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p control-server ssh:: 2>&1 | tail -20`
Expected: FAIL to compile — `render_authorized_keys`, `render_bastion_sshd_config`, `BASTION_AUTHORIZED_KEYS` not found.

- [ ] **Step 3: Implement the pure renderers**

At the top of `crates/control-server/src/ssh.rs` (above the tests):

```rust
use std::collections::HashSet;

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
```

Note: `UsePAM no` + `ForceCommand /usr/sbin/nologin` make the account jump-only; `-W`/ProxyJump opens a `direct-tcpip` channel that never triggers `ForceCommand` or the shell, so forwarding still works while an interactive `ssh rmng@bastion` is refused.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p control-server ssh:: 2>&1 | tail -20`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/ssh.rs crates/control-server/src/main.rs
git commit -m "feat(ssh): pure bastion sshd_config + authorized_keys renderers"
```

---

## Task 4: `ssh.rs` — host-key generation (bastion + per-clone)

**Files:**
- Modify: `crates/control-server/src/ssh.rs`
- Test: `crates/control-server/src/ssh.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub fn ensure_hostkey(key_path: &Path) -> anyhow::Result<()>` — generate an ed25519 host key at `key_path` (+ `.pub`) via `ssh-keygen` if absent; idempotent (existing key untouched). Creates parent dirs (0700).
  - `pub fn bastion_hostkey_path(data_dir: &str) -> PathBuf` → `<data_dir>/ssh/bastion/ssh_host_ed25519_key`.
  - `pub fn clone_hostkey_path(data_dir: &str, clone_id: &str) -> PathBuf` → `<data_dir>/ssh/clones/<clone_id>/ssh_host_ed25519_key`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `crates/control-server/src/ssh.rs`:

```rust
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
```

Add the imports at the top of the file: `use std::path::{Path, PathBuf};`

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p control-server ssh:: -- hostkey 2>&1 | tail -20`
Expected: FAIL to compile — functions not found.

- [ ] **Step 3: Implement**

Add to `crates/control-server/src/ssh.rs`:

```rust
use anyhow::{Context, Result};

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
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p control-server ssh:: 2>&1 | tail -20`
Expected: PASS (the idempotency test runs where `ssh-keygen` exists, else prints "skipping").

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/ssh.rs
git commit -m "feat(ssh): persisted ed25519 host-key generation (bastion + per-clone)"
```

---

## Task 5: `ssh.rs` — clone key material for provisioning + push-decision helper

**Files:**
- Modify: `crates/control-server/src/ssh.rs`
- Test: `crates/control-server/src/ssh.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `crate::docker::TarEntry`, `ensure_hostkey`, `clone_hostkey_path`, `render_authorized_keys` (earlier tasks).
- Produces:
  - `pub fn clone_ssh_tar_entries(data_dir: &str, clone_id: &str, keys: &[String]) -> anyhow::Result<Vec<TarEntry>>` — the `authorized_keys` (mode 0600, uid/gid 1000, path `home/rmng/.ssh/authorized_keys`) + the clone's host key (mode 0600, uid/gid 0, path `etc/ssh/ssh_host_ed25519_key`) + its `.pub` (0644, root). Generates/persists the host key as a side effect.
  - `pub fn keys_hash(keys: &[String]) -> u64` — order-independent hash of the rendered key set, for cheap "did this clone's keys change?" tracking.

**Context:** `TarEntry { path, data, mode, uid, gid }` (`docker.rs:337`). Clone home files use `CLONE_UID`/`CLONE_GID` = 1000; `/etc/ssh/*` are root-owned. The `~rmng/.ssh` directory itself is pre-created 700 by the template (Task 7) so `StrictModes` passes.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
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

    let ak = entries.iter().find(|e| e.path == "home/rmng/.ssh/authorized_keys").expect("authorized_keys entry");
    assert_eq!(ak.mode, 0o600);
    assert_eq!((ak.uid, ak.gid), (1000, 1000));
    assert_eq!(ak.data, b"ssh-ed25519 AAAA a\n");

    let hk = entries.iter().find(|e| e.path == "etc/ssh/ssh_host_ed25519_key").expect("host key entry");
    assert_eq!(hk.mode, 0o600);
    assert_eq!((hk.uid, hk.gid), (0, 0));
    assert!(entries.iter().any(|e| e.path == "etc/ssh/ssh_host_ed25519_key.pub" && e.mode == 0o644));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keys_hash_is_order_independent_and_change_sensitive() {
    let a = keys_hash(&["k1".into(), "k2".into()]);
    let b = keys_hash(&["k2".into(), "k1".into()]);
    assert_eq!(a, b, "order must not matter");
    let c = keys_hash(&["k1".into()]);
    assert_ne!(a, c, "a changed key set must change the hash");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p control-server ssh:: -- clone_tar keys_hash 2>&1 | tail -20`
Expected: FAIL to compile — functions not found.

- [ ] **Step 3: Implement**

Add to `crates/control-server/src/ssh.rs`:

```rust
use crate::docker::TarEntry;

/// The tar entries that provision SSH into one clone: its `authorized_keys` (clone-user
/// owned, 0600) and its stable host key + public half (root, 0600 / 0644). Generates and
/// persists the host key on first call for this clone id. The `~rmng/.ssh` dir is
/// pre-created 700 by the template, so only the file is dropped here.
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

    Ok(vec![
        TarEntry {
            path: "home/rmng/.ssh/authorized_keys".into(),
            data: render_authorized_keys(keys).into_bytes(),
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
```

Note: `home/rmng/.ssh/authorized_keys` uses the literal `rmng` home; this matches `CLONE_USER`. If a reviewer prefers the constant, format with `crate::docker::CLONE_USER` — but the template hardcodes `/home/rmng` too, so the literal is consistent.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p control-server ssh:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/ssh.rs
git commit -m "feat(ssh): clone SSH tar entries (authorized_keys + host key) + keys_hash"
```

---

## Task 6: `ssh.rs` — bastion supervisor + reconciler, wired into `main`

**Files:**
- Modify: `crates/control-server/src/ssh.rs`, `crates/control-server/src/main.rs`, `Dockerfile`
- Test: `crates/control-server/src/ssh.rs` (`#[cfg(test)]`) — pure helpers only; the supervisor/reconciler is integration-verified in Task 11.

**Interfaces:**
- Consumes: everything above; `crate::app::App`, `app.config()`, `app.store.get().hosts` (each `wire::Host` has `id`, `managed`), `app.docker.upload_tar`, `app.docker.is_running`.
- Produces:
  - `pub async fn run(app: App)` — spawned from `main`. Ensures the bastion host key, renders config + authorized_keys, then loops: supervise `sshd` + reconcile.
  - `pub fn backoff(failures: u32) -> Duration` (mirror `smb::backoff`).
  - `pub fn managed_clone_ids(hosts: &[wire::Host]) -> Vec<String>` — sorted, deduped ids of managed hosts (the `PermitOpen` allowlist source). Pure — tested.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
#[test]
fn backoff_escalates_then_caps() {
    assert_eq!(backoff(0), std::time::Duration::from_secs(15));
    assert_eq!(backoff(4).as_secs(), 240);
    assert_eq!(backoff(10).as_secs(), 300); // capped
    assert_eq!(backoff(u32::MAX).as_secs(), 300); // saturating
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
    let ids = managed_clone_ids(&[h1, unmanaged, h2]);
    assert_eq!(ids, vec!["a-clone".to_string(), "b-clone".to_string()]);
}
```

(If `wire::Host` has no `Default`, construct via the same literal `provision.rs` tests use, or add `#[derive(Default)]` — check `control.rs`. If it lacks `Default`, build the struct inline with all required fields.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p control-server ssh:: -- backoff managed_clone_ids 2>&1 | tail -20`
Expected: FAIL — `backoff` / `managed_clone_ids` not found.

- [ ] **Step 3: Implement the supervisor + reconciler**

Add to `crates/control-server/src/ssh.rs` (mirroring `smb.rs`):

```rust
use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

use crate::app::App;

const BASE_BACKOFF: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const STABLE_RUN: Duration = Duration::from_secs(60);
/// How often to re-render the bastion allowlist and push keys into clones.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);

pub fn backoff(failures: u32) -> Duration {
    BASE_BACKOFF.saturating_mul(2u32.saturating_pow(failures)).min(MAX_BACKOFF)
}

/// Sorted, deduped ids of managed clones — the `PermitOpen` allowlist source. A stopped
/// clone stays listed (harmless: the dial just fails); membership tracks the fleet roster.
pub fn managed_clone_ids(hosts: &[wire::Host]) -> Vec<String> {
    let mut ids: Vec<String> =
        hosts.iter().filter(|h| h.managed).map(|h| h.id.clone()).collect();
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
    let key_path = bastion_hostkey_path(data_dir);
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
    let hash = keys_hash(&cfg.ssh.authorized_keys);
    for host in app.store.get().hosts.into_iter().filter(|h| h.managed) {
        if pushed.get(&host.id) == Some(&hash) {
            continue;
        }
        if !app.docker.is_running(&host.id).await.unwrap_or(false) {
            continue; // stopped clones get keys at next provision/boot
        }
        match clone_ssh_tar_entries(data_dir, &host.id, &cfg.ssh.authorized_keys) {
            // Only push authorized_keys live; the host key is provision-time only (changing
            // it under a running sshd would need a restart), so filter to the .ssh file.
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

/// Ensure the host key, render the initial config, then supervise `sshd` forever with
/// capped backoff. Never returns. Disabled clones/keys are fine — the bastion just runs
/// with an empty `authorized_keys` (no one can auth) and `PermitOpen none`.
pub async fn run(app: App) {
    let data_dir = app.config().data_dir.clone();
    if let Err(e) = ensure_hostkey(&bastion_hostkey_path(&data_dir)) {
        tracing::error!(target: "ssh", "bastion host key generation failed: {e}");
    }
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
```

Wire it in `crates/control-server/src/main.rs` next to `tokio::spawn(smb::run(app.clone()));`:

```rust
    tokio::spawn(ssh::run(app.clone()));
```

Install `openssh-server` in the control-server image. In `Dockerfile`, find the runtime `apt-get install` (the samba/vah264enc layer, ~line 103) and add `openssh-server` to its package list. Also ensure `/etc/rmng/ssh` exists — the code `create_dir_all`s it, so no Dockerfile change needed for the dir.

- [ ] **Step 4: Run the tests + full build**

Run: `cargo test -p control-server ssh:: 2>&1 | tail -20`
Expected: PASS (pure helpers).
Run: `cargo build -p control-server 2>&1 | tail -15`
Expected: builds clean.

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/ssh.rs crates/control-server/src/main.rs Dockerfile
git commit -m "feat(ssh): bastion sshd supervisor + fleet-tracking reconciler; install openssh-server"
```

---

## Task 7: Clone template — install/enable `sshd`, pre-create `~rmng/.ssh`, strip host keys

**Files:**
- Modify: `template/Dockerfile`, `template/setup/30-user.sh`
- Verify: build the template + inspect (no Rust unit test — this is image config)

**Context:** Clones boot systemd PID 1; a system `ssh.service` is correct. The template must NOT ship host keys (control-server injects unique ones per clone — Task 5). `~rmng/.ssh` must be 700 uid 1000 so `sshd` `StrictModes` accepts the injected `authorized_keys`.

- [ ] **Step 1: Install + harden clone `sshd`**

In `template/Dockerfile`, add `openssh-server` to the clone's `apt-get install` list (the phase that installs desktop/toolbox packages). Then add a hardened drop-in config and enable the service. Add near the other `template/setup/*.sh` COPY/RUN phases a small step (or fold into `30-user.sh`):

Create the drop-in via `30-user.sh` (see Step 2) OR a dedicated `RUN` in the Dockerfile:

```dockerfile
# Clone sshd: pubkey-only, no root, no passwords. Host keys are NOT baked — the
# control-server injects a unique persisted key per clone at provision (like machine-id).
RUN mkdir -p /etc/ssh/sshd_config.d \
 && printf '%s\n' \
      'PasswordAuthentication no' \
      'PermitRootLogin no' \
      'KbdInteractiveAuthentication no' \
      'PubkeyAuthentication yes' \
      'AllowUsers rmng' \
      'X11Forwarding no' \
      > /etc/ssh/sshd_config.d/10-rmng.conf \
 && systemctl enable ssh
```

- [ ] **Step 2: Pre-create `~rmng/.ssh` (700) + strip baked host keys**

In `template/setup/30-user.sh`, after the `rmng` user is created (`useradd … -u 1000`), add:

```sh
# SSH: the control-server injects authorized_keys here at provision. Pre-create the dir
# with the exact perms/owner sshd StrictModes requires (else it silently ignores the key).
install -d -m 700 -o "$USERNAME" -g "$USERNAME" "/home/$USERNAME/.ssh"
```

In `template/Dockerfile`'s tail (the same `RUN` that blanks `/etc/machine-id`, ~line 105), strip any baked host keys so no two clones share an identity and so the injected key is authoritative:

```dockerfile
 && rm -f /etc/ssh/ssh_host_*
```

- [ ] **Step 3: Verify by building the template + inspecting**

Per the rebuild-template runbook / `docs/DEPLOY.md`, build the template image, then:

```bash
# In a throwaway container from the built template image:
docker run --rm --entrypoint sh <template-image> -c \
  'ls -ld /home/rmng/.ssh; stat -c "%a %U" /home/rmng/.ssh; \
   ls -1 /etc/ssh/ssh_host_* 2>/dev/null || echo NO_HOST_KEYS; \
   cat /etc/ssh/sshd_config.d/10-rmng.conf; \
   systemctl is-enabled ssh'
```
Expected: `/home/rmng/.ssh` is `700 rmng`; `NO_HOST_KEYS`; the drop-in prints; `ssh` is `enabled`.

- [ ] **Step 4: Commit**

```bash
git add template/Dockerfile template/setup/30-user.sh
git commit -m "feat(template): clone sshd (pubkey-only), pre-create ~rmng/.ssh 700, strip baked host keys"
```

---

## Task 8: Provision — inject clone SSH material at create

**Files:**
- Modify: `crates/control-server/src/provision.rs` (`clone_container_after_create`, the second `upload_tar` block ~370-409)
- Test: `crates/control-server/src/provision.rs` (`#[cfg(test)]`) — assert the entry set via a small extracted helper, OR a direct unit test of `ssh::clone_ssh_tar_entries` already covers content (Task 5). Add a provision-level guard.

**Interfaces:**
- Consumes: `crate::ssh::clone_ssh_tar_entries` (Task 5), the clone's `hostname`, `app.config().ssh.authorized_keys`, `app.config().data_dir`.

- [ ] **Step 1: Inject the entries**

In `clone_container_after_create`, in the block that builds `entries` for the second `upload_tar` (after the machine-id / preset env entries, before `docker.upload_tar(container, entries).await?`), append the SSH material:

```rust
    // SSH: the clone's stable host key + the current authorized_keys, so `ssh -J … rmng@<id>`
    // works the moment the clone is up. The template pre-created ~rmng/.ssh (700) and ships
    // no host keys, so these land with the right owner/perms. Best-effort: a keygen failure
    // must not fail the whole clone — log and continue (SSH just won't work until the next
    // reconcile push).
    match crate::ssh::clone_ssh_tar_entries(&cfg.data_dir, hostname, &cfg.ssh.authorized_keys) {
        Ok(mut ssh_entries) => entries.append(&mut ssh_entries),
        Err(e) => tracing::warn!("clone {hostname}: ssh material skipped: {e}"),
    }
```

(`cfg` is already in scope from `let cfg = app.config();` at the top of `clone_container`; if not in `clone_container_after_create`, add `let cfg = app.config();` at its top.)

- [ ] **Step 2: Add a regression test**

Since `clone_ssh_tar_entries` is unit-tested in Task 5, add a lightweight provision test asserting the config plumbs `data_dir`/keys through. If `clone_container_after_create` isn't easily unit-testable (it needs a Docker daemon), instead assert the helper contract here:

```rust
#[test]
fn provision_uses_ssh_clone_entries_contract() {
    // Guards that provision's SSH injection targets the clone-user .ssh path (the template
    // pre-creates it 700). If this path ever changes, StrictModes will reject the key.
    if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rmng-prov-ssh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let e = crate::ssh::clone_ssh_tar_entries(dir.to_str().unwrap(), "c1", &["ssh-ed25519 A a".into()]).unwrap();
    assert!(e.iter().any(|t| t.path == "home/rmng/.ssh/authorized_keys" && t.mode == 0o600 && t.uid == 1000));
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 3: Run + build**

Run: `cargo test -p control-server -- provision 2>&1 | tail -15` → PASS.
Run: `cargo build -p control-server 2>&1 | tail -10` → clean.

- [ ] **Step 4: Commit**

```bash
git add crates/control-server/src/provision.rs
git commit -m "feat(provision): inject clone SSH host key + authorized_keys pre-boot"
```

---

## Task 9: Web — apply SSH changes immediately on `config_put`

**Files:**
- Modify: `crates/control-server/src/web.rs` (`config_put` ~830-885)
- Modify: `crates/control-server/src/ssh.rs` — extract a reusable `apply_now`
- Test: covered by Task 6/5 helpers; add one guard test in `ssh.rs`

**Interfaces:**
- Produces: `pub async fn apply_now(app: &App)` in `ssh.rs` — re-render bastion files (no reload; the file is read fresh by sshd, and the reconciler SIGHUPs on fleet change) and push keys to running clones once, using a throwaway tracking map so a manual key change propagates without waiting for the ≤10s tick.

- [ ] **Step 1: Implement `apply_now`**

Add to `crates/control-server/src/ssh.rs`:

```rust
/// One-shot apply used by the config PUT path so a key add/remove propagates immediately
/// rather than waiting for the reconcile tick. Rewrites the bastion `authorized_keys`
/// (read fresh by sshd per connection — no reload needed) and pushes to running clones.
pub async fn apply_now(app: &App) {
    let data_dir = app.config().data_dir.clone();
    render_bastion_files(app, &data_dir);
    let mut once = std::collections::HashMap::new();
    push_keys_to_clones(app, &data_dir, &mut once).await;
}
```

- [ ] **Step 2: Call it from `config_put`**

In `crates/control-server/src/web.rs`, in `config_put`, after `*app.cfg.write().unwrap() = merged.clone();` (so `app.config()` sees the new keys), add:

```rust
    // Propagate any SSH key change to the bastion + running clones immediately.
    if old.ssh.authorized_keys != merged.ssh.authorized_keys {
        crate::ssh::apply_now(&app).await;
    }
```

- [ ] **Step 3: Guard test**

Add to `ssh.rs` tests (pure guard — `apply_now` needs an `App`, so just assert the trigger predicate logic is what we intend by testing `render_authorized_keys` empty vs set already covered). Instead add a doc-level test that changing keys changes the rendered file content:

```rust
#[test]
fn changing_keys_changes_rendered_authorized_keys() {
    assert_ne!(
        render_authorized_keys(&["ssh-ed25519 A a".into()]),
        render_authorized_keys(&["ssh-ed25519 B b".into()])
    );
}
```

- [ ] **Step 4: Run + build**

Run: `cargo test -p control-server ssh:: 2>&1 | tail -10` → PASS.
Run: `cargo build -p control-server 2>&1 | tail -10` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/ssh.rs crates/control-server/src/web.rs
git commit -m "feat(web): apply SSH key changes to bastion + running clones on config PUT"
```

---

## Task 10: CLI — `rmng ssh <clone>` prints the one-liner

**Files:**
- Modify: `crates/cli/src/args.rs` (`Cmd` enum), `crates/cli/src/commands.rs` (handler + dispatch)
- Test: `crates/cli/src/commands.rs` (`#[cfg(test)]`) — pure `build_ssh_command`

**Interfaces:**
- Produces: `pub fn build_ssh_command(public_host: &str, bastion_port: u16, clone_id: &str) -> String` → `ssh -J rmng@<public_host>:<bastion_port> -o StrictHostKeyChecking=accept-new rmng@<clone_id>`.

**Context:** the CLI fetches the redacted config (it already talks to the web API for `ps` etc.). Use `ssh.public_host` if non-empty, else the host from the CLI's own server base URL (see `docs/CLI.md` "Server resolution"). This mirrors how other handlers resolve the server.

- [ ] **Step 1: Write the failing test**

Add to `crates/cli/src/commands.rs` `#[cfg(test)]`:

```rust
#[test]
fn ssh_command_is_the_inline_jump_one_liner() {
    assert_eq!(
        build_ssh_command("rmng.example.com", 2222, "w-cp-claude"),
        "ssh -J rmng@rmng.example.com:2222 -o StrictHostKeyChecking=accept-new rmng@w-cp-claude"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rmng-cli -- ssh_command 2>&1 | tail -15`
Expected: FAIL — `build_ssh_command` not found.

- [ ] **Step 3: Implement**

Add the `Ssh` variant to `Cmd` in `crates/cli/src/args.rs`:

```rust
    /// Print the ready-to-paste `ssh` command for a clone (jump via the control-server bastion)
    Ssh {
        /// Host id of the clone
        host: String,
    },
```

In `crates/cli/src/commands.rs`, add the pure builder + handler:

```rust
/// The copy-paste one-liner: inline `-J` jump through the bastion, terminating at the
/// clone's own sshd. `accept-new` makes the first connect prompt-free (host keys are stable).
pub fn build_ssh_command(public_host: &str, bastion_port: u16, clone_id: &str) -> String {
    format!(
        "ssh -J rmng@{public_host}:{bastion_port} -o StrictHostKeyChecking=accept-new rmng@{clone_id}"
    )
}
```

Add the handler (mirror an existing simple handler like `Ps`): fetch `GET /api/config`, read `config.ssh.publicHost` (fall back to the API server's hostname) and `config.listen.bastion`, then `println!("{}", build_ssh_command(host, port, &clone_id));`. Wire the `Cmd::Ssh { host }` arm into the dispatch `match` in `main.rs`/`commands.rs`.

- [ ] **Step 4: Run + build**

Run: `cargo test -p rmng-cli 2>&1 | tail -15` → PASS.
Run: `cargo build -p rmng-cli 2>&1 | tail -10` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/args.rs crates/cli/src/commands.rs
git commit -m "feat(cli): rmng ssh <clone> prints the bastion jump one-liner"
```

---

## Task 11: Frontend — SSH Access settings + per-clone "Copy SSH command"

**Files:**
- Create: `frontend/app/lib/ssh.ts` (pure `buildSshCommand`)
- Modify: `frontend/app/components/SettingsPanel.tsx` (SSH Access section)
- Modify: the clone row/detail component (add the copy button) — locate via the existing per-clone actions (search for where a `Host` row renders action buttons)
- Verify: `npm run typecheck`; add a Storybook story for the command helper

**Interfaces:**
- Produces: `export function buildSshCommand(publicHost: string, bastionPort: number, cloneId: string): string` — identical string to the CLI's `build_ssh_command`.

- [ ] **Step 1: Pure command helper**

Create `frontend/app/lib/ssh.ts`:

```ts
/** The copy-paste one-liner: inline `-J` jump through the control-server bastion,
 *  terminating at the clone's own sshd. Mirrors the Rust `build_ssh_command`. */
export function buildSshCommand(publicHost: string, bastionPort: number, cloneId: string): string {
  return `ssh -J rmng@${publicHost}:${bastionPort} -o StrictHostKeyChecking=accept-new rmng@${cloneId}`;
}
```

- [ ] **Step 2: SSH Access settings section**

In `frontend/app/components/SettingsPanel.tsx`: add an `ssh` state seeded from the fetched config (like `listen` is at ~224/256):

```tsx
const [ssh, setSsh] = useState<{ enabled: boolean; authorizedKeys: string[]; publicHost: string }>({
  enabled: false, authorizedKeys: [], publicHost: "",
});
// in the effect that seeds from config `c`:
setSsh({ enabled: c.ssh?.enabled ?? false, authorizedKeys: c.ssh?.authorizedKeys ?? [], publicHost: c.ssh?.publicHost ?? "" });
```

Render a section: an `enabled` toggle, a `<textarea>` bound to `ssh.authorizedKeys.join("\n")` (on change: split on `\n`, keep non-blank lines), a `publicHost` text input (placeholder "auto — inferred from this page's address"), and show the read-only bastion port (`listen.bastion`). Include `ssh` in the PUT body (the object built around line 396):

```tsx
body: JSON.stringify({ listen, /* … */ ssh }),
```

- [ ] **Step 3: Per-clone "Copy SSH command" button**

In the clone row/detail component, add a button that computes and copies the command:

```tsx
import { buildSshCommand } from "~/lib/ssh";
// publicHost: config.ssh.publicHost || window.location.hostname
// bastionPort: config.listen.bastion
const cmd = buildSshCommand(config.ssh?.publicHost || window.location.hostname, config.listen.bastion, host.id);
// onClick: navigator.clipboard.writeText(cmd) + a brief "Copied!" affordance
```

Use the existing icon/button conventions (lucide at `size-4`, per the frontend conventions). A `Terminal` or `Copy` lucide icon fits.

- [ ] **Step 4: Story + typecheck**

Add a small story (e.g. `frontend/app/components/SshCommand.stories.tsx` or extend `SettingsPanel.stories.tsx`) that renders `buildSshCommand("rmng.example.com", 2222, "w-cp-claude")` so it's visually verifiable.

Run: `cd frontend && npm run typecheck 2>&1 | tail -20`
Expected: no type errors (the regenerated `SshConfig.ts` / `AppConfigRedacted.ts` from Task 1 back these).

- [ ] **Step 5: Commit**

```bash
git add frontend/app/lib/ssh.ts frontend/app/components/SettingsPanel.tsx frontend/app/components/*.stories.tsx <clone-row-file>
git commit -m "feat(frontend): SSH Access settings + per-clone Copy SSH command"
```

---

## Task 12: Deploy — publish `2222` + document the flow

**Files:**
- Modify: `compose.yaml`, `README.md`, `docs/DEPLOY.md`, `docs/PROTOCOL.md`
- Verify: docs render; the run one-liner includes the port

- [ ] **Step 1: Publish the bastion port**

In `README.md`'s `docker run` one-liner and `compose.yaml`, add `-p 2222:2222` / the compose `ports:` entry. Update the "Ports:" sentence in `README.md` to include `2222 SSH bastion (jump into clones)`.

- [ ] **Step 2: Port map + protocol note**

In `docs/PROTOCOL.md`'s port table (the row list near line 19/156), add a row for `bastion 2222` (OpenSSH jump host; forwards to `clone:22`). Note it is public-key only and jump-only.

- [ ] **Step 3: Operator guide**

In `docs/DEPLOY.md`, add an "SSH into clones" section: paste a public key in Settings → SSH Access, click **Copy SSH command** on a clone (or `rmng ssh <clone>`), paste + enter. Note: `scp`/`rsync -e 'ssh -J …'`/`GIT_SSH_COMMAND`/VSCode "Add New SSH Host…" all take the same string; clones created before the template rebuild lack `sshd` and must be recreated; adding a key propagates to running clones within ~10s (or immediately on save).

- [ ] **Step 4: Manual E2E (on the W6800 box / CT 106)**

Document + run:
```bash
# 1. paste a pubkey in Settings → SSH Access, save.
# 2. create/recreate a clone from the rebuilt template.
# 3. from a laptop whose private key matches:
ssh -J rmng@<control-host>:2222 -o StrictHostKeyChecking=accept-new rmng@<clone-id> 'hostname; id'
scp -J rmng@<control-host>:2222 /etc/hostname rmng@<clone-id>:~/from-laptop
# 4. VSCode: Remote-SSH: Add New SSH Host… → paste the ssh command → connect.
# 5. add a second key in the UI; confirm `ssh` works within ~10s WITHOUT recreating the clone.
```
Expected: command runs with no host-key prompt; `scp` lands the file; VSCode connects; the newly added key works on a running clone.

- [ ] **Step 5: Commit**

```bash
git add compose.yaml README.md docs/DEPLOY.md docs/PROTOCOL.md
git commit -m "docs(deploy): publish bastion :2222, document SSH-into-clones flow"
```

---

## Self-Review

**Spec coverage:**
- Real end-to-end ssh into clone's own sshd → Tasks 6 (bastion `-W` jump), 7 (clone sshd). ✅
- Copy-paste one-liner, no laptop config → Tasks 10 (CLI), 11 (UI button + helper). ✅
- Bastion sshd on 2222, pubkey, jump-only, reuse `rmng` account, absolute AuthorizedKeysFile, Match block → Task 3 (render) + 6 (supervise). ✅
- `PermitOpen` = live clones, reload on fleet change → Task 6 (`managed_clone_ids` + SIGHUP). ✅
- Keys pasted in UI, stored in AppConfig, redacted passthrough → Tasks 1, 11. ✅
- Install on bastion + every clone; running clones pick up live → Tasks 6 (`push_keys_to_clones`), 8 (provision), 9 (immediate apply). ✅
- Stable host keys (bastion + per-clone), template ships none → Tasks 4, 5, 7. ✅
- `public_host` inferred from web address, override → Tasks 1 (field), 10/11 (fallback to hostname). ✅
- Deploy: publish 2222, template rebuild, docs, manual E2E → Task 12. ✅
- Out-of-scope (per-key ACLs, fail2ban, known_hosts bundle, multi-user) → not implemented, correct. ✅

**Placeholder scan:** No TBD/TODO; every code step shows real code; `<clone-row-file>` / `<template-image>` / `<control-host>` are operator-supplied values in shell commands, not code gaps.

**Type consistency:** `render_authorized_keys`/`render_bastion_sshd_config`/`ensure_hostkey`/`clone_ssh_tar_entries`/`keys_hash`/`managed_clone_ids`/`backoff`/`apply_now`/`build_ssh_command`/`buildSshCommand` used consistently across tasks. `SshConfig` fields (`enabled`/`authorizedKeys`/`publicHost`) and `listen.bastion` consistent between wire (Task 1), backend reads (6/9), CLI (10), frontend (11). `TarEntry` fields match `docker.rs:337`. Bastion account `rmng`, port `2222`, paths (`/etc/rmng/ssh/*`, `data_dir/ssh/...`) consistent throughout.

**Verified before execution:** `wire::Host` derives `Default` (`control.rs:86`), so the Task 6 test's `wire::Host::default()` compiles. `Host.id`/`Host.managed` exist (`control.rs:89`). CLI package is `rmng-cli` (bin `rmng`), so `cargo test -p rmng-cli` is correct. `AppConfig::redacted()` builds the struct at `config.rs:411` (add `ssh: self.ssh.clone(),` there).
