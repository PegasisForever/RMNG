//! Per-group CLIProxyAPI instance supervisor, per-clone router-key store, and management
//! helpers for the group-proxy model (see
//! `docs/superpowers/specs/2026-07-19-cliproxy-group-proxy-plan.md`).
//!
//! Each account group (`wire::Group`) is backed by ONE CLIProxyAPI process — the
//! `cliproxy-sidecar` binary the runtime image ships — bound to loopback on its own port,
//! with its own `data/cliproxy/<group>/{config.yaml, auth/}`.
//!
//! **Process split (see [`crate::groupproxy`]).** The supervisor + the `/cc` router used to
//! run inside the control-server, which meant every control-server upgrade killed every
//! in-flight agent request in the fleet. They now live in a separate long-lived container
//! (`rmng-cliproxy`) running this same image under the `group-proxy` subcommand. That splits
//! the ownership of this module in two:
//!   - **group-proxy container**: spawns/supervises the sidecars ([`run`]), routes clone
//!     traffic to them, and forwards admin traffic to their loopback management APIs. It
//!     opens [`CliProxyManager`] READ-ONLY ([`CliProxyManager::load_read_only`]) so two
//!     processes never race on `data/cliproxy-instances.json`.
//!   - **control-server**: sole writer of `data/cliproxy-instances.json` — it allocates each
//!     group's port + secrets ([`apply_now`]), mints per-clone router keys, mints the shared
//!     admin secret, polls per-group usage from the shared `auth-dir`s, and reaches the
//!     instances only through the group-proxy's admin surface ([`group_catalog`],
//!     `web.rs::mgmt_send_retry`).
//!
//! CLIProxyAPI owns token refresh + intra-group account selection (session-affinity +
//! per-model quota failover); rmng owns only lifecycle, routing, and display. The plaintext
//! management secret + inbound key are minted here and persisted in
//! `data/cliproxy-instances.json` — NOT re-read from `config.yaml`, which CLIProxyAPI
//! bcrypt-hashes in place on startup.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use wire::AppConfig;

use crate::app::App;

/// Loopback host every instance binds — never published; only the in-container router dials
/// it. The sidecar's config sets `host: "127.0.0.1"`.
const BIND_HOST: &str = "127.0.0.1";
/// First port handed to a group instance; each new group takes the lowest free port at or
/// above this that no other instance holds.
const PORT_BASE: u16 = 9100;
/// The sidecar binary the runtime image ships (Dockerfile `go-build` stage → runtime COPY).
const SIDECAR_BIN: &str = "/usr/local/bin/cliproxy-sidecar";
/// Supervisor reconcile cadence (spawn new groups / stop removed ones).
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
/// Restart backoff (mirrors `ssh.rs`): first retry after BASE, doubling to MAX; a run that
/// stays up past STABLE_RUN resets the counter.
const BASE_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const STABLE_RUN: Duration = Duration::from_secs(30);

fn backoff(failures: u32) -> Duration {
    BASE_BACKOFF.saturating_mul(2u32.saturating_pow(failures)).min(MAX_BACKOFF)
}

/// A filesystem-safe group name (a path component + a valid CLIProxyAPI auth label). Group
/// names come from operator config; reject anything that could escape the data dir.
pub fn safe_group(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && name != "."
        && name != ".."
}

/// 32 random bytes, hex-encoded — a bearer/api secret. Reads `/dev/urandom` (Linux target),
/// so no crate dependency and cryptographically strong.
fn random_token() -> String {
    let mut buf = [0u8; 32];
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)) {
        Ok(()) => buf.iter().map(|b| format!("{b:02x}")).collect(),
        // /dev/urandom should never be missing on Linux; if it is, fail loud rather than
        // mint a predictable secret.
        Err(e) => panic!("cliproxy: cannot read /dev/urandom for secret generation: {e}"),
    }
}

/// Persisted per-group instance identity: a stable port + the secrets rmng minted. Kept here
/// (NOT re-read from the instance's `config.yaml`, which the sidecar bcrypt-hashes on
/// startup) so the plaintext management secret + inbound key survive restarts unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceMeta {
    port: u16,
    /// Inbound `api-keys` value the router presents to this instance.
    inbound_key: String,
    /// Plaintext management secret (sent as `X-Management-Key`).
    mgmt_secret: String,
}

/// On-disk state (`data/cliproxy-instances.json`, `0600`): instance identities, per-clone
/// router keys, and the shared control-server ↔ group-proxy admin secret. Never enters
/// `state.json`/`/events` — it holds secrets. The control-server is the ONLY writer; the
/// group-proxy container opens the same file read-only and reloads it on a short cadence.
#[derive(Default, Serialize, Deserialize)]
struct InstancesFile {
    #[serde(default)]
    instances: HashMap<String, InstanceMeta>,
    /// `host_id` → per-clone router bearer key (the clone's `ANTHROPIC_AUTH_TOKEN`).
    #[serde(default)]
    router_keys: HashMap<String, String>,
    /// The shared secret authenticating the control-server ↔ group-proxy admin channel (the
    /// group-proxy's `/admin/*` forward surface and the control-server's token-delta intake).
    /// Lives here rather than in `config.json` because it is a minted secret, and `config.json`
    /// round-trips through the Settings UI. Absent in files written before the process split;
    /// [`CliProxyManager::ensure_admin_secret`] mints one on first use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admin_secret: Option<String>,
}

struct Inner {
    file: InstancesFile,
    /// key → host_id reverse index for router request auth.
    token_index: HashMap<String, String>,
    data_dir: String,
    /// True in the group-proxy container. The control-server is the sole writer of
    /// `cliproxy-instances.json`; a second writer would clobber a concurrently-minted router
    /// key or admin secret (the file is rewritten whole). Read-only managers therefore never
    /// persist and never allocate — they pick changes up via [`CliProxyManager::reload`].
    read_only: bool,
}

/// Shared supervisor + key state hung off `App` (control-server) or off the group-proxy's own
/// context ([`SupervisorCtx`]).
pub struct CliProxyManager {
    inner: Mutex<Inner>,
    /// Wakes the by-group usage poller for an immediate poll (account added / manual refresh).
    usage_poke: Arc<tokio::sync::Notify>,
}

/// The inputs the per-group supervisor + the router need, decoupled from [`App`] so the
/// group-proxy container — which has no Docker client, media plane, or state store — can run
/// them. Both entrypoints compile the same [`run`]/[`supervise_group`] code; only this context
/// differs.
#[derive(Clone)]
pub struct SupervisorCtx {
    pub cliproxy: Arc<CliProxyManager>,
    /// Live view of `config.json`, refreshed from the shared `/data` volume by the group-proxy's
    /// own watcher (see [`crate::groupproxy`]).
    pub cfg: Arc<RwLock<AppConfig>>,
}

impl SupervisorCtx {
    fn data_dir(&self) -> String {
        self.cfg.read().unwrap().data_dir.clone()
    }
    fn groups(&self) -> Vec<wire::Group> {
        self.cfg.read().unwrap().groups.clone()
    }
}

impl CliProxyManager {
    /// Load persisted instance meta + router keys from `data/cliproxy-instances.json`, as the
    /// file's sole WRITER (the control-server).
    pub fn load(data_dir: &str) -> Self {
        Self::open(data_dir, false)
    }

    /// Same file, opened read-only for the group-proxy container: never persists, never
    /// allocates a port/secret, and re-reads the file via [`Self::reload`] so control-server
    /// writes (a new group's meta, a new clone's router key) are picked up within a second.
    pub fn load_read_only(data_dir: &str) -> Self {
        Self::open(data_dir, true)
    }

    fn open(data_dir: &str, read_only: bool) -> Self {
        let file = Self::read_file(data_dir);
        let token_index =
            file.router_keys.iter().map(|(host, key)| (key.clone(), host.clone())).collect();
        Self {
            inner: Mutex::new(Inner {
                file,
                token_index,
                data_dir: data_dir.to_string(),
                read_only,
            }),
            usage_poke: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Parse the instances file, or `None` when it is absent/unreadable/malformed. Distinguished
    /// from "empty" so [`Self::reload`] can hold the previous contents rather than blanking every
    /// key on a transient miss.
    fn read_file_opt(data_dir: &str) -> Option<InstancesFile> {
        std::fs::read(Self::state_path(data_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    fn read_file(data_dir: &str) -> InstancesFile {
        Self::read_file_opt(data_dir).unwrap_or_default()
    }

    /// Re-read `cliproxy-instances.json` from the shared volume and swap it in wholesale.
    /// Only meaningful for a read-only manager (the writer's in-memory copy is authoritative).
    /// Cheap enough to call on a 1 s cadence: the file is a few KB and only grows with the
    /// clone count.
    ///
    /// A failed read/parse KEEPS the previous contents. The control-server writes atomically
    /// (temp + rename), so this shouldn't happen — but blanking every router key and the admin
    /// secret would 401 the whole fleet for a tick, and "keep serving what we last knew" is the
    /// strictly better failure mode for a file we don't own.
    pub fn reload(&self) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.read_only {
            return;
        }
        let Some(file) = Self::read_file_opt(&inner.data_dir) else {
            return;
        };
        inner.token_index =
            file.router_keys.iter().map(|(host, key)| (key.clone(), host.clone())).collect();
        inner.file = file;
    }

    /// Wake the by-group usage poller for an immediate poll — called right after an account is
    /// added to a group or on a manual refresh, so the account/usage shows up in ~a second
    /// instead of at the next scheduled poll. Multiple pokes coalesce into one poll.
    pub fn poke_usage(&self) {
        self.usage_poke.notify_one();
    }

    /// The notify handle the poller awaits between cycles.
    pub(crate) fn usage_poke_handle(&self) -> Arc<tokio::sync::Notify> {
        self.usage_poke.clone()
    }

    pub(crate) fn state_path(data_dir: &str) -> PathBuf {
        PathBuf::from(data_dir).join("cliproxy-instances.json")
    }

    /// The shared control-server ↔ group-proxy admin secret, minting + persisting one on first
    /// use. Called by the control-server before it launches / talks to the group-proxy
    /// container, so the secret exists on disk by the time the sidecar container reads it.
    ///
    /// This secret gates BOTH directions of the internal channel: the group-proxy's
    /// `/admin/*` management-forward surface (which a clone must never reach — a clone only
    /// holds a per-clone router bearer) and the control-server's `/internal/tokens` delta
    /// intake. One secret rather than two because both ends are the same trust domain and a
    /// second one would just double the rotation surface.
    pub fn ensure_admin_secret(&self) -> String {
        let mut inner = self.inner.lock().unwrap();
        if let Some(secret) = inner.file.admin_secret.clone() {
            return secret;
        }
        let secret = random_token();
        inner.file.admin_secret = Some(secret.clone());
        Self::persist(&inner);
        secret
    }

    /// The admin secret if one has been minted, WITHOUT minting (the read-only group-proxy
    /// side, which cannot write the file). `None` until the control-server has run
    /// [`Self::ensure_admin_secret`] — the group-proxy then refuses every `/admin/*` request
    /// rather than falling open.
    pub fn admin_secret(&self) -> Option<String> {
        self.inner.lock().unwrap().file.admin_secret.clone()
    }

    /// Atomic `0600` persist of `Inner.file`. Best-effort (logs on failure). A no-op for a
    /// read-only manager — the group-proxy container must never write this file.
    fn persist(inner: &Inner) {
        if inner.read_only {
            return;
        }
        let path = Self::state_path(&inner.data_dir);
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let json = match serde_json::to_vec_pretty(&inner.file) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(target: "cliproxy", "serialize instances state: {e}");
                return;
            }
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&tmp, &json)
            .and_then(|()| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)))
            .and_then(|()| std::fs::rename(&tmp, &path))
        {
            tracing::error!(target: "cliproxy", "persist instances state: {e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Ensure a group has a stable port + secrets, allocating + persisting on first use.
    /// On a read-only manager this only reads — the group-proxy waits for the control-server
    /// to allocate rather than minting a second, conflicting identity for the same group.
    fn ensure_meta(&self, group: &str) -> Option<InstanceMeta> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(meta) = inner.file.instances.get(group) {
            return Some(meta.clone());
        }
        if inner.read_only {
            return None;
        }
        let used: std::collections::HashSet<u16> =
            inner.file.instances.values().map(|m| m.port).collect();
        let port = (PORT_BASE..u16::MAX).find(|p| !used.contains(p)).unwrap_or(PORT_BASE);
        let meta = InstanceMeta {
            port,
            inbound_key: random_token(),
            mgmt_secret: random_token(),
        };
        inner.file.instances.insert(group.to_string(), meta.clone());
        Self::persist(&inner);
        Some(meta)
    }

    fn meta(&self, group: &str) -> Option<InstanceMeta> {
        self.inner.lock().unwrap().file.instances.get(group).cloned()
    }

    /// Allocate + persist a group's identity, as the supervisor's first reconcile pass would.
    /// Test-only surface for sibling modules ([`crate::groupproxy`]'s router tests), which need
    /// a provisioned group without spawning a real sidecar.
    #[cfg(test)]
    pub(crate) fn ensure_meta_for_test(&self, group: &str) {
        let _ = self.ensure_meta(group);
    }

    /// Loopback port for a group's instance, if one has been provisioned. `None` while a
    /// group has no instance yet → the router answers 503 and the clone's agent retries.
    pub fn port_for(&self, group: &str) -> Option<u16> {
        self.meta(group).map(|m| m.port)
    }

    /// Inbound api-key the router must present to a group's instance.
    pub fn inbound_key_for(&self, group: &str) -> Option<String> {
        self.meta(group).map(|m| m.inbound_key)
    }

    /// `(base management URL, plaintext X-Management-Key)` for a group's instance, as dialed
    /// from INSIDE the group-proxy container. E.g. `http://127.0.0.1:9100/v0/management`.
    /// The control-server can no longer reach this (the instances are loopback-only in another
    /// container) and goes through the admin-forward surface instead — see
    /// [`crate::groupproxy::admin_mgmt_url`].
    pub fn management(&self, group: &str) -> Option<(String, String)> {
        self.meta(group)
            .map(|m| (format!("http://{BIND_HOST}:{}/v0/management", m.port), m.mgmt_secret))
    }

    /// Absolute `auth-dir` for a group's instance (where credential JSON files live).
    pub fn auth_dir(&self, group: &str) -> PathBuf {
        let data_dir = self.inner.lock().unwrap().data_dir.clone();
        auth_dir_path(&data_dir, group)
    }

    // ---- per-clone router keys -------------------------------------------------------

    /// The per-clone bearer key, minting + persisting one on first request. Injected into the
    /// clone as `ANTHROPIC_AUTH_TOKEN` / the Codex+OpenCode provider key; the router maps it
    /// back to the clone id. Stable for the clone's life (a group change never rotates it).
    pub fn mint_router_key(&self, host_id: &str) -> String {
        let mut inner = self.inner.lock().unwrap();
        if let Some(key) = inner.file.router_keys.get(host_id) {
            return key.clone();
        }
        let key = random_token();
        inner.file.router_keys.insert(host_id.to_string(), key.clone());
        inner.token_index.insert(key.clone(), host_id.to_string());
        Self::persist(&inner);
        key
    }

    /// Resolve a presented bearer token to the owning clone id (router request auth).
    pub fn clone_for_token(&self, token: &str) -> Option<String> {
        self.inner.lock().unwrap().token_index.get(token).cloned()
    }

    /// Drop a clone's router key on delete so a stale key can never route again.
    pub fn forget_clone(&self, host_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(key) = inner.file.router_keys.remove(host_id) {
            inner.token_index.remove(&key);
            Self::persist(&inner);
        }
    }
}

fn group_dir(data_dir: &str, group: &str) -> PathBuf {
    PathBuf::from(data_dir).join("cliproxy").join(group)
}
fn auth_dir_path(data_dir: &str, group: &str) -> PathBuf {
    group_dir(data_dir, group).join("auth")
}
fn config_path(data_dir: &str, group: &str) -> PathBuf {
    group_dir(data_dir, group).join("config.yaml")
}

/// The `config.yaml` an instance runs with. Static per group — accounts are OAuth files in
/// `auth-dir`, added via the management API, not listed here. `session-affinity: true` +
/// `max-retry-credentials: 0` are what give per-clone stickiness and per-(account,model)
/// quota failover (the Fable case). The sidecar bcrypt-hashes `secret-key` in place on
/// startup; we always know the plaintext from `InstanceMeta`.
fn render_config_yaml(meta: &InstanceMeta, auth_dir: &str) -> String {
    let mut body = format!(
        "# Managed by RMNG (cliproxy.rs). Regenerated on every (re)spawn.\n\
         host: \"{BIND_HOST}\"\n\
         port: {port}\n\
         auth-dir: \"{auth_dir}\"\n\
         api-keys:\n  - \"{inbound}\"\n\
         remote-management:\n  allow-remote: false\n  secret-key: \"{secret}\"\n  disable-control-panel: true\n\
         routing:\n  strategy: \"round-robin\"\n  session-affinity: true\n  session-affinity-ttl: \"6h\"\n\
         quota-exceeded:\n  switch-project: true\n\
         max-retry-credentials: 0\n",
        port = meta.port,
        inbound = meta.inbound_key,
        secret = meta.mgmt_secret,
    );
    // Blacklist the operator-excluded models from this instance's /v1/models catalog — this is
    // what shapes Claude Code's gateway-discovery picker. (OpenCode/Codex are separately held to
    // GPT-only by their own generated client configs; see clone_reconcile.rs::RMNG_GPT_MODELS.)
    body.push_str(&oauth_excluded_models_yaml());
    body
}

/// Models hidden from every group instance's `/v1/models` catalog. Per OAuth channel;
/// `oauth-excluded-models` accepts exact ids + `*` wildcards (prefix/suffix/substring) and is
/// listing-scoped. KEPT: claude-opus-5 / claude-opus-4-8 / claude-sonnet-5 / claude-haiku-4-5 / claude-fable-5
/// (claude) and the gpt-5.6 tiers + gpt-5.5 (codex).
const EXCLUDED_CLAUDE_MODELS: &[&str] = &[
    "*-4-7",
    "*-4-6",
    "*-4-5-20251101",
    "*-4-20250514",
    "*-4-1-20250805",
    "*-4-5-20250929",
    "claude-3-7-sonnet*",
    "claude-3-5-haiku*",
];
const EXCLUDED_CODEX_MODELS: &[&str] = &[
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex-spark",
    "codex-auto-review",
    "gpt-image-1.5",
    "gpt-image-2",
];
/// Antigravity (Google Code Assist) is a multi-model surface: besides `gemini-*` it also
/// serves Claude (`claude-opus-4-6-thinking`, `claude-sonnet-4-6`) and OpenAI OSS
/// (`gpt-oss-*`). We keep Antigravity as a **Gemini-only** channel — the `claude-*` / `gpt-*`
/// prefixes drop every non-Gemini model it offers (now and future) without touching the real
/// `anthropic`/`openai` channels (exclusions are keyed per serving provider). The remaining
/// `gemini-*` entries hide the low-effort / flash / lite / image tiers; KEPT: gemini-3-flash-agent
/// and gemini-pro-agent.
const EXCLUDED_ANTIGRAVITY_MODELS: &[&str] = &[
    "claude-*",
    "gpt-*",
    "gemini-3-flash",
    "gemini-3.1-flash-image",
    "gemini-3.1-flash-lite",
    "gemini-3.1-pro-low",
    "gemini-3.5-flash-extra-low",
    "gemini-3.5-flash-low",
];

/// Render the `oauth-excluded-models:` block for a generated `config.yaml`.
fn oauth_excluded_models_yaml() -> String {
    let mut s = String::from("oauth-excluded-models:\n  claude:\n");
    for m in EXCLUDED_CLAUDE_MODELS {
        s.push_str(&format!("    - \"{m}\"\n"));
    }
    s.push_str("  codex:\n");
    for m in EXCLUDED_CODEX_MODELS {
        s.push_str(&format!("    - \"{m}\"\n"));
    }
    s.push_str("  antigravity:\n");
    for m in EXCLUDED_ANTIGRAVITY_MODELS {
        s.push_str(&format!("    - \"{m}\"\n"));
    }
    s
}

/// Write `config.yaml` + ensure `auth-dir` exists (both under `data/cliproxy/<group>/`).
/// `config.yaml` is `0600` (holds the inbound key + management secret).
fn write_instance_files(data_dir: &str, group: &str, meta: &InstanceMeta) -> std::io::Result<PathBuf> {
    let auth = auth_dir_path(data_dir, group);
    // Creating the auth dir also creates its parent `data/cliproxy/<group>/`.
    std::fs::create_dir_all(&auth)?;
    let cfg = config_path(data_dir, group);
    let body = render_config_yaml(meta, &auth.to_string_lossy());
    std::fs::write(&cfg, body)?;
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600))?;
    Ok(cfg)
}

fn spawn_sidecar(config: &PathBuf) -> std::io::Result<Child> {
    Command::new(SIDECAR_BIN)
        .arg("--config")
        .arg(config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Abort of the per-group supervisor task drops the Child → SIGKILL, so a removed
        // group's instance dies with its supervisor.
        .kill_on_drop(true)
        .spawn()
}

async fn log_lines<R: AsyncRead + Unpin>(reader: R, group: String) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => tracing::info!(target: "cliproxy", "[{group}] {line}"),
            Ok(None) => break,
            Err(_) => continue,
        }
    }
}

/// Supervise ONE group's instance forever (until the task is aborted when the group is
/// deleted). Regenerates `config.yaml` before each (re)spawn, drains logs, waits for exit,
/// and respawns with capped backoff. Mirrors `ssh::run` for a single dynamic child.
///
/// Runs in the group-proxy container. `ensure_meta` is read-only there, so a group whose meta
/// the control-server hasn't allocated yet simply retries on the reconcile cadence instead of
/// minting a conflicting port/secret pair.
async fn supervise_group(ctx: SupervisorCtx, group: String) {
    let data_dir = ctx.data_dir();
    let Some(meta) = ctx.cliproxy.ensure_meta(&group) else {
        tracing::debug!(
            target: "cliproxy",
            "[{group}] no instance meta yet (the control-server allocates it) — retrying"
        );
        tokio::time::sleep(RECONCILE_INTERVAL).await;
        return;
    };
    let mut failures: u32 = 0;
    let mut spawn_err_logged = false;
    loop {
        let cfg = match write_instance_files(&data_dir, &group, &meta) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(target: "cliproxy", "[{group}] write instance files: {e}");
                tokio::time::sleep(backoff(failures)).await;
                failures = failures.saturating_add(1);
                continue;
            }
        };
        let started = Instant::now();
        match spawn_sidecar(&cfg) {
            Ok(mut child) => {
                spawn_err_logged = false;
                tracing::info!(target: "cliproxy", "[{group}] instance listening on {BIND_HOST}:{}", meta.port);
                let out = child.stdout.take();
                let err = child.stderr.take();
                let logs = async {
                    tokio::join!(
                        async { if let Some(r) = out { log_lines(r, group.clone()).await } },
                        async { if let Some(r) = err { log_lines(r, group.clone()).await } },
                    );
                };
                tokio::select! {
                    status = child.wait() => match status {
                        Ok(s) => tracing::warn!(target: "cliproxy", "[{group}] instance exited ({s}) — restarting"),
                        Err(e) => tracing::warn!(target: "cliproxy", "[{group}] waiting on instance failed: {e}"),
                    },
                    _ = logs => {}
                }
            }
            Err(e) if !spawn_err_logged => {
                tracing::error!(target: "cliproxy", "[{group}] failed to spawn sidecar ({SIDECAR_BIN}): {e}");
                spawn_err_logged = true;
            }
            Err(e) => tracing::debug!(target: "cliproxy", "[{group}] sidecar spawn still failing: {e}"),
        }
        if started.elapsed() >= STABLE_RUN {
            failures = 0;
        }
        let delay = backoff(failures);
        failures = failures.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
}

/// Top-level supervisor: reconcile the running per-group tasks against `config.groups` every
/// tick. New groups get a spawned `supervise_group`; removed groups have their task aborted
/// (which drops the Child → SIGKILL via `kill_on_drop`). Spawned by the group-proxy container's
/// entrypoint ([`crate::groupproxy::main`]), NOT by the control-server — moving it out of the
/// control-server process is the whole point of the split (a control-server upgrade must not
/// take the inference path with it).
pub async fn run(ctx: SupervisorCtx) {
    let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        let desired: Vec<String> = ctx
            .groups()
            .iter()
            .map(|g| g.name.clone())
            .filter(|n| {
                if safe_group(n) {
                    true
                } else {
                    tracing::warn!(target: "cliproxy", "ignoring group with unsafe name {n:?}");
                    false
                }
            })
            .collect();

        // Stop instances for groups that no longer exist, and reap tasks that returned. A
        // supervisor returns in exactly one case: the group has no instance meta yet, because
        // the control-server hasn't allocated it (we're read-only on that file). Reaping it
        // here lets the `or_insert_with` below respawn it on the next tick, which is the retry.
        let stale: Vec<(String, bool)> = tasks
            .iter()
            .filter(|(g, h)| !desired.contains(g) || h.is_finished())
            .map(|(g, _)| (g.clone(), desired.contains(g)))
            .collect();
        for (g, still_wanted) in stale {
            if let Some(handle) = tasks.remove(&g) {
                handle.abort();
                if still_wanted {
                    tracing::debug!(target: "cliproxy", "[{g}] supervisor returned — respawning next tick");
                } else {
                    tracing::info!(target: "cliproxy", "[{g}] instance stopped (group removed)");
                }
            }
        }

        // Start instances for new groups.
        for g in desired {
            let ctx = ctx.clone();
            tasks.entry(g.clone()).or_insert_with(|| tokio::spawn(supervise_group(ctx, g)));
        }

        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

/// Immediate reconcile hint after a `groups` config change, run in the CONTROL-SERVER. It
/// allocates + persists each configured group's port and secrets so they are on the shared
/// volume before the group-proxy container's next reconcile tick reads them (the group-proxy
/// is read-only on that file and never allocates). The actual spawn/teardown happens in
/// [`run`], over in the sidecar container.
pub fn apply_now(app: &App) {
    for g in app.config().groups.iter().filter(|g| safe_group(&g.name)) {
        let _ = app.cliproxy.ensure_meta(&g.name);
    }
    // The admin secret gates the control-server's own management-forward calls into the
    // group-proxy, so mint it on the same beat as the per-group identities.
    let _ = app.cliproxy.ensure_admin_secret();
}

// --- by-group usage poller -------------------------------------------------------------

/// Usage poll cadence — matches the account pollers' default (`ClaudeConfig::poll_secs`).
const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(600);

/// One provider account parsed out of an instance's `auth-dir` credential JSON. Files are
/// named `claude-<email>.json` / `codex-<email>.json`; fields are read tolerantly (unknown
/// keys ignored). The token is used as-is — RMNG never refreshes it (CLIProxyAPI owns
/// refresh); an expired token just yields a 401 → `stale`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuthAccount {
    /// `"claude"`, `"codex"`, or `"antigravity"` (from the file's `type`, else inferred from
    /// the file name prefix).
    pub kind: String,
    pub email: String,
    pub access_token: String,
    /// Codex only — the ChatGPT account id sent as `ChatGPT-Account-Id`.
    pub account_id: Option<String>,
}

#[derive(Deserialize)]
struct RawAuthFile {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

/// Parse one `auth-dir` credential file body + its file name into an [`AuthAccount`]. `kind`
/// comes from the JSON `type` when present, else the file-name prefix
/// (`claude-`/`codex-`/`antigravity-`).
/// `None` when the body has no access token or we can't determine an email.
pub(crate) fn parse_auth_file(file_name: &str, body: &str) -> Option<AuthAccount> {
    let raw: RawAuthFile = serde_json::from_str(body).ok()?;
    let access_token = raw.access_token.filter(|t| !t.is_empty())?;
    let kind = raw
        .r#type
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            if file_name.starts_with("codex-") {
                "codex".to_string()
            } else if file_name.starts_with("antigravity-") {
                "antigravity".to_string()
            } else {
                "claude".to_string()
            }
        });
    // Prefer the JSON email; fall back to the `<kind>-<email>.json` file-name stem.
    let email = raw.email.filter(|e| !e.is_empty()).or_else(|| {
        file_name
            .strip_suffix(".json")
            .and_then(|s| s.split_once('-').map(|(_, e)| e.to_string()))
            .filter(|e| !e.is_empty())
    })?;
    Some(AuthAccount { kind, email, access_token, account_id: raw.account_id.filter(|a| !a.is_empty()) })
}

/// Enumerate the accounts authenticated into a group's `auth-dir` (its `*.json` credential
/// files). Best-effort: an unreadable dir/file is skipped. Not sorted.
pub(crate) fn read_auth_accounts(auth_dir: &std::path::Path) -> Vec<AuthAccount> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(auth_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Some(acct) = parse_auth_file(&file_name, &body) {
                out.push(acct);
            }
        }
    }
    out
}

/// Background poller (spawned in `main.rs` alongside the account pollers): every
/// `USAGE_POLL_INTERVAL`, read each group's `auth-dir`, fetch usage per account REUSING the
/// account pollers' parsers (`claude`/`codex` `fetch_usage_view`), and publish
/// `ControlState.usage_groups`. Never refreshes tokens (CLIProxyAPI owns refresh); a 401
/// surfaces as `stale` from the per-(group, account) last-good cache. Leaves the old flat
/// `claude_accounts` pollers running untouched.
pub async fn run_usage_poller(app: App) {
    tracing::info!(target: "cliproxy", "group usage poller started (every {}s)", USAGE_POLL_INTERVAL.as_secs());
    // Per-(group|email) last-good view, so a transient 401/timeout shows the previous numbers
    // marked `stale` instead of blanking the account.
    let mut last_good: HashMap<String, wire::ClaudeUsage> = HashMap::new();
    let poke = app.cliproxy.usage_poke_handle();
    loop {
        poll_usage_once(&app, &mut last_good).await;
        // Wake early on a poke (account added / manual refresh); otherwise poll on the timer.
        tokio::select! {
            _ = tokio::time::sleep(USAGE_POLL_INTERVAL) => {}
            _ = poke.notified() => {
                tracing::debug!(target: "cliproxy", "usage poll poked (account change / manual refresh)");
            }
        }
    }
}

async fn poll_usage_once(app: &App, last_good: &mut HashMap<String, wire::ClaudeUsage>) {
    let groups: Vec<String> = app
        .config()
        .groups
        .iter()
        .map(|g| g.name.clone())
        .filter(|n| safe_group(n))
        .collect();

    let mut out: Vec<wire::GroupUsage> = Vec::with_capacity(groups.len());
    for group in &groups {
        let auth_dir = app.cliproxy.auth_dir(group);
        let accounts = read_auth_accounts(&auth_dir);
        let mut views = Vec::with_capacity(accounts.len());
        for acct in accounts {
            // Group- AND provider-scoped-unique id: the same email can be authenticated into
            // several groups (independent token sets per instance) and, within one group, under
            // more than one provider (e.g. Gemini + Claude for the same address). Both `group`
            // and `kind` are needed or the two rows collide (breaks React keys + drag reorder).
            let id = format!("{group}|{}|{}", acct.kind, acct.email);
            let fetched = match acct.kind.as_str() {
                "codex" => {
                    let account_id = acct.account_id.clone().unwrap_or_default();
                    crate::codex::fetch_usage_view(
                        &app.http,
                        id.clone(),
                        acct.email.clone(),
                        true,
                        &acct.access_token,
                        &account_id,
                    )
                    .await
                }
                // Antigravity (Gemini) has no pollable usage endpoint — publish a display-only
                // presence row. No network call, always `Ok`. See `crate::antigravity`.
                "antigravity" => {
                    Ok(crate::antigravity::usage_view(id.clone(), acct.email.clone(), true))
                }
                _ => {
                    crate::claude::fetch_usage_view(
                        &app.http,
                        id.clone(),
                        acct.email.clone(),
                        true,
                        &acct.access_token,
                    )
                    .await
                }
            };
            match fetched {
                Ok(u) => {
                    last_good.insert(id.clone(), u.clone());
                    views.push(u);
                }
                Err(e) => {
                    // Reuse the previous good numbers marked stale; else a bare error row.
                    let msg = e.to_string();
                    views.push(match last_good.get(&id).cloned() {
                        Some(mut prev) => {
                            prev.stale = Some(true);
                            prev.last_updated = crate::clone_ops::now_ms();
                            prev
                        }
                        None => wire::ClaudeUsage {
                            id,
                            email: acct.email.clone(),
                            provider: Some(match acct.kind.as_str() {
                                "codex" => wire::Provider::Codex,
                                "antigravity" => wire::Provider::Antigravity,
                                _ => wire::Provider::Claude,
                            }),
                            active: true,
                            assignable: None,
                            error: Some(msg),
                            stale: Some(true),
                            last_updated: crate::clone_ops::now_ms(),
                            five_hour: None,
                            seven_day: None,
                            fable: None,
                            spend: None,
                            reset_credits: None,
                        },
                    });
                }
            }
        }
        views.sort_by(|a, b| a.email.cmp(&b.email));
        out.push(wire::GroupUsage { name: group.clone(), accounts: views });
    }

    // Drop cache entries for groups/accounts that no longer exist, so it can't grow forever.
    let live: std::collections::HashSet<String> =
        out.iter().flat_map(|g| g.accounts.iter().map(|a| a.id.clone())).collect();
    last_good.retain(|k, _| live.contains(k));

    app.store.mutate(|s| s.usage_groups = out);
}

// --- live model catalog ----------------------------------------------------------------

/// Every model id in an OpenAI-style `/v1/models` body (`{"data":[{"id":"..."},...]}`) — BOTH
/// the claude and gpt/codex channels. An instance's catalog is already blacklist-filtered
/// (`oauth-excluded-models`), so this is exactly the set a group serves. Deduped + stably sorted;
/// a body that doesn't parse (or carries no `data` array) yields an empty vec. Pure so it can be
/// unit-tested, and the shared base for [`gpt_ids_from_models_json`].
fn ids_from_models_json(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(data) = v.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = data
        .iter()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The non-`claude-` subset of [`ids_from_models_json`] — dropping the Claude channel leaves
/// exactly the GPT/codex-channel models OpenCode/Codex are allowed to pick. Deduped + stably
/// sorted (inherited from [`ids_from_models_json`]); a body that doesn't parse yields an empty
/// vec. Pure so it can be unit-tested.
///
/// The reconciler no longer calls this directly — it caches the full [`group_catalog`] once per
/// group and derives the GPT list from that `Vec` (the same `!starts_with("claude-")` split) so a
/// group is queried only once per pass — but this stays as the documented JSON-boundary filter.
#[allow(dead_code)] // retained pure filter for the /v1/models GPT split; exercised by its test
fn gpt_ids_from_models_json(body: &str) -> Vec<String> {
    ids_from_models_json(body).into_iter().filter(|id| !id.starts_with("claude-")).collect()
}

/// The FULL live model catalog (both `claude-` and gpt/codex ids) a group's CLIProxyAPI instance
/// advertises: GET its `/v1/models` and return every id (see [`ids_from_models_json`]).
/// Because the catalog is already blacklist-filtered by `oauth-excluded-models`, this is exactly
/// the set a group serves — new models appear here automatically and only the operator blacklist
/// removes them. The reconciler derives BOTH the OpenCode/Codex GPT list (the non-`claude-` ids)
/// and Claude Code's default model (`clone_reconcile::default_claude_model`) from this one fetch,
/// so a group is queried at most once per reconcile pass.
///
/// The instances bind loopback INSIDE the group-proxy container, so this goes through that
/// container's admin catalog surface (`GET /admin/:group/v1/models`, authenticated with the
/// shared admin secret) rather than dialing `127.0.0.1:<port>` directly.
///
/// Returns `vec![]` on any miss — no instance/port/key yet, the group-proxy container still
/// starting, a timeout, a non-2xx, or an empty catalog — and the caller falls back to
/// `clone_reconcile::FALLBACK_GPT_MODELS` (Codex/OpenCode) / `FALLBACK_CLAUDE_MODEL` (Claude Code).
pub(crate) async fn group_catalog(app: &App, group: &str) -> Vec<String> {
    let Some(secret) = app.cliproxy.admin_secret() else {
        return Vec::new();
    };
    let url = crate::groupproxy::admin_catalog_url(group);
    let body = async {
        let resp = app
            .http
            .get(&url)
            .header(crate::groupproxy::ADMIN_HEADER, &secret)
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.text().await.ok()
    }
    .await;
    body.map(|b| ids_from_models_json(&b)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_group_rules() {
        assert!(safe_group("team-a"));
        assert!(safe_group("pool_1.beta"));
        assert!(!safe_group(""));
        assert!(!safe_group(".."));
        assert!(!safe_group("a/b"));
        assert!(!safe_group("bad name"));
    }

    #[test]
    fn config_yaml_has_the_load_bearing_knobs() {
        let meta = InstanceMeta { port: 9100, inbound_key: "IK".into(), mgmt_secret: "MS".into() };
        let y = render_config_yaml(&meta, "/data/cliproxy/g/auth");
        assert!(y.contains("host: \"127.0.0.1\""));
        assert!(y.contains("port: 9100"));
        assert!(y.contains("auth-dir: \"/data/cliproxy/g/auth\""));
        assert!(y.contains("session-affinity: true"));
        assert!(y.contains("max-retry-credentials: 0"));
        assert!(y.contains("- \"IK\""));
        assert!(y.contains("secret-key: \"MS\""));
        // The model blacklist shapes the /v1/models catalog (Claude Code's discovery picker).
        assert!(y.contains("oauth-excluded-models:"));
        assert!(y.contains("claude-3-5-haiku*"));
        assert!(y.contains("gpt-5.4-mini"));
    }

    #[test]
    fn ports_are_stable_and_unique_per_group() {
        let dir = std::env::temp_dir().join(format!("cliproxy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = CliProxyManager::load(&dir.to_string_lossy());
        let a1 = mgr.ensure_meta("a").unwrap().port;
        let b = mgr.ensure_meta("b").unwrap().port;
        let a2 = mgr.ensure_meta("a").unwrap().port;
        assert_eq!(a1, a2, "same group keeps its port");
        assert_ne!(a1, b, "distinct groups get distinct ports");
        assert!(a1 >= PORT_BASE && b >= PORT_BASE);
    }

    /// The group-proxy container opens the instances file READ-ONLY: it must never allocate a
    /// group identity (the control-server owns that, and two allocators would hand the same
    /// group two different ports/secrets) and never write the file (whole-file rewrites would
    /// clobber a concurrently-minted router key). It DOES pick up the writer's changes on
    /// `reload`.
    #[test]
    fn read_only_manager_never_allocates_and_reloads_writer_changes() {
        let dir = std::env::temp_dir().join(format!("cliproxy-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.to_string_lossy().into_owned();

        let ro = CliProxyManager::load_read_only(&data_dir);
        assert!(ro.ensure_meta("g").is_none(), "read-only must not allocate");
        assert!(ro.admin_secret().is_none(), "read-only must not mint the admin secret");
        assert!(
            !CliProxyManager::state_path(&data_dir).exists(),
            "read-only must not create the instances file"
        );

        // The writer allocates; the reader sees it only after a reload.
        let rw = CliProxyManager::load(&data_dir);
        let port = rw.ensure_meta("g").unwrap().port;
        let secret = rw.ensure_admin_secret();
        let key = rw.mint_router_key("h1");
        assert_eq!(ro.port_for("g"), None, "stale until reload");
        ro.reload();
        assert_eq!(ro.port_for("g"), Some(port));
        assert_eq!(ro.admin_secret().as_deref(), Some(secret.as_str()));
        assert_eq!(ro.clone_for_token(&key).as_deref(), Some("h1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The admin secret is minted once and then stable across process restarts — a rotation on
    /// every boot would break every in-flight group-proxy ↔ control-server call.
    #[test]
    fn admin_secret_is_minted_once_and_persists() {
        let dir = std::env::temp_dir().join(format!("cliproxy-admin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.to_string_lossy().into_owned();
        let first = CliProxyManager::load(&data_dir).ensure_admin_secret();
        assert!(!first.is_empty());
        assert_eq!(
            CliProxyManager::load(&data_dir).ensure_admin_secret(),
            first,
            "a reloaded manager must reuse the persisted secret"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_auth_file_reads_claude_and_codex_shapes() {
        let claude = parse_auth_file(
            "claude-a@b.com.json",
            r#"{"id_token":"x","access_token":"AT","refresh_token":"RT","email":"a@b.com","type":"claude","expired":false}"#,
        )
        .unwrap();
        assert_eq!(claude.kind, "claude");
        assert_eq!(claude.email, "a@b.com");
        assert_eq!(claude.access_token, "AT");
        assert!(claude.account_id.is_none());

        let codex = parse_auth_file(
            "codex-z@o.com.json",
            r#"{"access_token":"AT2","email":"z@o.com","type":"codex","account_id":"acc-1"}"#,
        )
        .unwrap();
        assert_eq!(codex.kind, "codex");
        assert_eq!(codex.account_id.as_deref(), Some("acc-1"));

        // No access token → not an account.
        assert!(parse_auth_file("claude-x.json", r#"{"email":"x@y"}"#).is_none());

        // Email + kind fall back to the file name when the JSON omits them.
        let inferred = parse_auth_file("codex-fallback@o.com.json", r#"{"access_token":"T"}"#).unwrap();
        assert_eq!(inferred.kind, "codex");
        assert_eq!(inferred.email, "fallback@o.com");

        // Unknown fields are tolerated.
        assert!(parse_auth_file("claude-a@b.json", r#"{"access_token":"T","email":"a@b","surprise":42}"#).is_some());
    }

    #[test]
    fn read_auth_accounts_skips_non_json_and_bad_files() {
        let dir = std::env::temp_dir().join(format!("cliproxy-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("claude-a@b.json"), r#"{"access_token":"AT","email":"a@b","type":"claude"}"#).unwrap();
        std::fs::write(dir.join("codex-c@d.json"), r#"{"access_token":"AT2","email":"c@d","type":"codex","account_id":"id1"}"#).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        let mut got = read_auth_accounts(&dir);
        got.sort_by(|a, b| a.email.cmp(&b.email));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].email, "a@b");
        assert_eq!(got[1].email, "c@d");
        assert_eq!(got[1].account_id.as_deref(), Some("id1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gpt_ids_from_models_json_drops_claude_keeps_gpt() {
        let body = r#"{"data":[
            {"id":"claude-opus-4-8"},
            {"id":"gpt-5.6-terra"},
            {"id":"claude-sonnet-5"},
            {"id":"gpt-5.5"},
            {"id":"gpt-5.6-sol"}
        ]}"#;
        let ids = gpt_ids_from_models_json(body);
        // Claude channel dropped; GPT channel kept, deduped + stably sorted.
        assert_eq!(ids, vec!["gpt-5.5", "gpt-5.6-sol", "gpt-5.6-terra"]);
        assert!(ids.iter().all(|id| !id.starts_with("claude-")));
        // Duplicates collapse.
        assert_eq!(
            gpt_ids_from_models_json(r#"{"data":[{"id":"gpt-5.5"},{"id":"gpt-5.5"}]}"#),
            vec!["gpt-5.5"]
        );
        // Malformed body / missing `data` array → empty.
        assert!(gpt_ids_from_models_json("not json").is_empty());
        assert!(gpt_ids_from_models_json(r#"{"object":"list"}"#).is_empty());
    }

    #[test]
    fn ids_from_models_json_keeps_both_channels() {
        let body = r#"{"data":[
            {"id":"gpt-5.6-terra"},
            {"id":"claude-opus-4-8"},
            {"id":"gpt-5.5"},
            {"id":"claude-sonnet-5"}
        ]}"#;
        // Both the claude and gpt channels are kept, deduped + stably sorted.
        assert_eq!(
            ids_from_models_json(body),
            vec!["claude-opus-4-8", "claude-sonnet-5", "gpt-5.5", "gpt-5.6-terra"]
        );
        // Duplicates collapse.
        assert_eq!(
            ids_from_models_json(r#"{"data":[{"id":"claude-opus-4-8"},{"id":"claude-opus-4-8"}]}"#),
            vec!["claude-opus-4-8"]
        );
        // Malformed body / missing `data` array → empty.
        assert!(ids_from_models_json("not json").is_empty());
        assert!(ids_from_models_json(r#"{"object":"list"}"#).is_empty());
    }

    #[test]
    fn router_keys_roundtrip_and_forget() {
        let dir = std::env::temp_dir().join(format!("cliproxy-rk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = CliProxyManager::load(&dir.to_string_lossy());
        let key = mgr.mint_router_key("host-1");
        assert_eq!(mgr.mint_router_key("host-1"), key, "stable per host");
        assert_eq!(mgr.clone_for_token(&key).as_deref(), Some("host-1"));
        mgr.forget_clone("host-1");
        assert_eq!(mgr.clone_for_token(&key), None);
    }
}
