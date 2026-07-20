//! Live migration for clones created by older control-server/template versions.
//!
//! New clones get current binaries and SSH material during `provision::clone_container`.
//! Existing running clones need an idempotent reconcile path so a control-server update can
//! make them operational without destructive recreate: install/enable clone-side sshd, refresh
//! the injected payload binaries, then restart only `rmng-clone-daemon` to pick up the daemon
//! binary. `agent-wrapper` is refreshed on disk but intentionally not restarted.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use crate::app::App;
use crate::docker::TarEntry;
use crate::files::is_safe_id;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const CLONE_UID: u64 = 1000;
const CLONE_GID: u64 = 1000;

const CODEX_AGENTS_MD: &str = r#"# Working in this clone

This machine is a **disposable, single-purpose dev sandbox** that belongs to you,
with **passwordless `sudo`**. Install packages, toolchains, and global CLIs freely
and reconfigure the system as needed — the machine itself is throwaway and there is
no other user to disturb. Optimize for getting the task done.

## When you're blocked

If you're genuinely stuck — missing access or credentials, an ambiguous
requirement, or a call that's the human's to make — **stop and ask** rather than
guessing or thrashing. A precise question beats a confident wrong turn.
"#;

fn payload_stamp_path() -> &'static str {
    "opt/rmng/.payload-hash"
}

fn ssh_stamp_path() -> &'static str {
    "etc/rmng/ssh-ready"
}

fn codex_parity_stamp_path() -> &'static str {
    "etc/rmng/codex-parity-hash"
}

/// Stamp marking the one-time group-proxy migration of a clone's dead provider credentials.
fn dead_creds_stamp_path() -> &'static str {
    "etc/rmng/group-proxy-migrated"
}

pub(crate) fn desired_payload_hash(entries: &[TarEntry]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for e in entries {
        e.path.hash(&mut h);
        e.mode.hash(&mut h);
        e.uid.hash(&mut h);
        e.gid.hash(&mut h);
        e.data.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

fn binary_payload_entries() -> Result<Vec<TarEntry>> {
    let mut entries = Vec::new();
    for b in crate::provision::CLONE_BINARIES {
        let data = crate::assets::payload(b.payload)
            .with_context(|| format!("payload {} is not staged", b.payload))?;
        entries.push(TarEntry {
            path: format!("{}/{}", b.dir, b.bin),
            data,
            mode: 0o755,
            uid: 0,
            gid: 0,
        });
    }
    Ok(entries)
}

fn payload_stamp_entry(hash: &str) -> TarEntry {
    TarEntry {
        path: payload_stamp_path().to_string(),
        data: format!("{hash}\n").into_bytes(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

pub(crate) fn payload_stamp_entry_for(entries: &[TarEntry]) -> TarEntry {
    payload_stamp_entry(&desired_payload_hash(entries))
}

pub(crate) fn ssh_stamp_entry() -> TarEntry {
    TarEntry {
        path: ssh_stamp_path().to_string(),
        data: b"ok\n".to_vec(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

/// The GPT model ids the RMNG group-proxy provider advertises to Codex + OpenCode. Codex/
/// OpenCode list GPT models only (never Claude), so their pickers can't surface a Claude
/// model — the soft per-agent visibility rule from the group-proxy plan. `[0]` is the default.
///
/// Current (2026-07) Codex GPT families: the GPT-5.6 tiers (sol/terra/luna, GA 2026-07-09) plus
/// the previous-generation `gpt-5.5`. The deprecated `-codex` suffix variants (`gpt-5.x-codex`)
/// are intentionally excluded. Ids per https://learn.chatgpt.com/docs/models; OpenAI's own
/// sample config uses `model = "gpt-5.6"` (https://learn.chatgpt.com/docs/config-file/config-sample).
/// TODO(cliproxy): the authoritative served ids are whatever the group's pinned CLIProxyAPI v7
/// instance advertises at `/v1/models` for a ChatGPT-subscription account — confirm this list
/// against that catalog. Visibility is soft: a mismatch only hides/omits a picker entry.
const RMNG_GPT_MODELS: &[&str] =
    &["gpt-5.6", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"];

fn codex_config_toml(clone_id: &str, control_mcp_url: Option<&str>, cc_base_url: Option<&str>) -> String {
    let mut body = String::from("# Managed by RMNG. Re-created by the control-server clone reconciler.\n\n");

    // Group-proxy provider (bare top-level keys MUST precede any [table] in TOML). When the
    // control host resolves, route Codex through the control-server's /cc/v1 OpenAI-compatible
    // surface and make it the active provider defaulting to a GPT model, so a Claude model can
    // never be picked from Codex. Additive: the old ~/.codex/auth.json push still runs; with
    // this provider active Codex uses RMNG_PROXY_KEY over the proxy instead.
    let cc_base = cc_base_url.map(str::trim).filter(|s| !s.is_empty());
    if let Some(base) = cc_base {
        body.push_str("model_provider = \"rmng\"\n");
        body.push_str(&format!("model = \"{}\"\n\n", RMNG_GPT_MODELS[0]));
        let _ = base; // used in the table below
    }

    body.push_str(
        r#"[mcp_servers.desktop]
url = "http://127.0.0.1:9004"

[mcp_servers.linear]
url = "https://mcp.linear.app/mcp"
bearer_token_env_var = "LINEAR_API_KEY"
"#,
    );

    if let Some(base) = cc_base {
        // The RMNG group-proxy provider. Schema per the Codex config reference
        // (https://learn.chatgpt.com/docs/config-file/config-reference and .../config-sample):
        //   - base_url ends in /v1; for the Responses wire protocol Codex appends `/responses`
        //     (so it POSTs `{base}/responses`, which the /cc router forwards to the instance).
        //   - wire_api = "responses" is the only supported value and matches the surface the
        //     instance serves.
        //   - env_key names the env var Codex reads at runtime and sends as the Bearer token
        //     (RMNG_PROXY_KEY, injected into /etc/environment per clone).
        //   - supports_websockets = false forces HTTP+SSE — it disables the Responses-API
        //     WebSocket transport, satisfying the plan's "Codex custom providers with WebSockets
        //     disabled" requirement (this is the real key; the sample config shows it commented).
        // requires_openai_auth is intentionally omitted (defaults false — we auth with the
        // env_key bearer, not a ChatGPT login). Codex has no per-provider model allow-list; the
        // single top-level `model` + `model_provider` above selects the default GPT model.
        body.push_str(&format!(
            r#"
[model_providers.rmng]
name = "RMNG"
base_url = "{base}"
wire_api = "responses"
env_key = "RMNG_PROXY_KEY"
supports_websockets = false
"#
        ));
    }

    if let Some(url) = control_mcp_url.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str(&format!(
            r#"
[mcp_servers."control-server"]
url = "{url}"
http_headers = {{ "x-rmng-clone" = "{clone_id}" }}
"#
        ));
    }
    body
}

/// The managed OpenCode config: a single OpenAI-compatible provider named `rmng` pointing at
/// the group-proxy router's /cc/v1 surface, keyed by RMNG_PROXY_KEY, listing GPT models only
/// (no Anthropic provider), so OpenCode's picker never shows a Claude model. `None` when the
/// control host can't be resolved (nothing to write this pass).
///
/// Schema per the OpenCode provider docs (https://opencode.ai/docs/providers):
///   - `npm = "@ai-sdk/openai-compatible"` is the custom OpenAI-compatible provider; it POSTs
///     `{baseURL}/chat/completions`, so `options.baseURL` ends in /v1 (the /cc router forwards
///     the suffix to the instance).
///   - `options.apiKey` accepts the `{env:VAR}` interpolation form (resolved from the clone env).
///   - the `models` map keys are the ids sent verbatim in the request `model` field.
///   - the top-level `model` sets the default as `"<provider>/<id>"`.
/// The global managed path is ~/.config/opencode/opencode.json. Same GPT-only id caveat as
/// [`RMNG_GPT_MODELS`] — the served set is ultimately the group instance's /v1/models.
fn opencode_config_json(cc_base_url: Option<&str>) -> Option<String> {
    let base = cc_base_url.map(str::trim).filter(|s| !s.is_empty())?;
    let models: serde_json::Map<String, serde_json::Value> = RMNG_GPT_MODELS
        .iter()
        .map(|m| (m.to_string(), serde_json::json!({ "name": m })))
        .collect();
    let cfg = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "model": format!("rmng/{}", RMNG_GPT_MODELS[0]),
        "provider": {
            "rmng": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "RMNG",
                "options": {
                    "baseURL": base,
                    "apiKey": "{env:RMNG_PROXY_KEY}"
                },
                "models": models
            }
        }
    });
    Some(serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".into()))
}

pub(crate) fn codex_parity_entries(
    clone_id: &str,
    control_mcp_url: Option<&str>,
    cc_base_url: Option<&str>,
) -> Vec<TarEntry> {
    let mut entries = vec![
        TarEntry {
            path: "home/rmng/.codex/AGENTS.md".to_string(),
            data: CODEX_AGENTS_MD.as_bytes().to_vec(),
            mode: 0o644,
            uid: CLONE_UID,
            gid: CLONE_GID,
        },
        TarEntry {
            path: "home/rmng/.codex/config.toml".to_string(),
            data: codex_config_toml(clone_id, control_mcp_url, cc_base_url).into_bytes(),
            mode: 0o600,
            uid: CLONE_UID,
            gid: CLONE_GID,
        },
    ];
    if let Some(json) = opencode_config_json(cc_base_url) {
        entries.push(TarEntry {
            path: "home/rmng/.config/opencode/opencode.json".to_string(),
            data: json.into_bytes(),
            mode: 0o600,
            uid: CLONE_UID,
            gid: CLONE_GID,
        });
    }
    entries
}

fn codex_parity_stamp_entry(hash: &str) -> TarEntry {
    TarEntry {
        path: codex_parity_stamp_path().to_string(),
        data: format!("{hash}\n").into_bytes(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

pub(crate) fn codex_parity_stamp_entry_for(entries: &[TarEntry]) -> TarEntry {
    codex_parity_stamp_entry(&desired_payload_hash(entries))
}

pub(crate) fn codex_prepare_script() -> &'static str {
    r#"set -e
install -d -o rmng -g rmng -m700 /home/rmng/.codex
install -d -o rmng -g rmng -m755 /home/rmng/.config /home/rmng/.config/opencode
mkdir -p /etc/rmng
"#
}

pub(crate) fn codex_cli_install_script() -> &'static str {
    r#"set -e
if ! runuser -u rmng -- bash -lc 'command -v codex >/dev/null 2>&1'; then
  runuser -u rmng -- bash -lc 'set -o pipefail; CODEX_NON_INTERACTIVE=1 curl -fsSL https://chatgpt.com/codex/install.sh | sh' \
    || { echo "codex install failed" >&2; exit 1; }
fi
"#
}

/// The group-proxy migration on an existing clone: delete the now-dead provider credential
/// files. Under the group-proxy model CLIProxyAPI owns tokens and clones reach inference only
/// through the `/cc` router (its env + agent configs are rewritten by the other reconcile
/// steps), so a clone must never carry its own `~/.claude/.credentials.json` /
/// `~/.codex/auth.json`. Idempotent (`rm -f`); combined with the additive env/config injection
/// this makes an existing clone work after its agent restarts — no container recreate.
fn dead_creds_cleanup_script() -> &'static str {
    r#"set -e
rm -f /home/rmng/.claude/.credentials.json /home/rmng/.codex/auth.json
"#
}

fn ssh_prepare_script() -> &'static str {
    r#"set -e
install -d -o rmng -g rmng -m700 /home/rmng/.ssh
mkdir -p /etc/ssh
"#
}

fn ssh_bootstrap_script() -> &'static str {
    r#"set -e
export DEBIAN_FRONTEND=noninteractive
if ! command -v sshd >/dev/null 2>&1; then
  apt-get update -qq
  apt-get install -y -qq openssh-server
fi
install -d -o rmng -g rmng -m700 /home/rmng/.ssh
if [ -f /home/rmng/.ssh/authorized_keys ]; then
  chown rmng:rmng /home/rmng/.ssh/authorized_keys
  chmod 600 /home/rmng/.ssh/authorized_keys
fi
mkdir -p /etc/ssh/sshd_config.d
mkdir -p /etc/rmng
cat > /etc/ssh/sshd_config.d/10-rmng.conf <<'RMNG_SSHD'
PasswordAuthentication no
PermitRootLogin no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AllowUsers rmng
X11Forwarding no
RMNG_SSHD
systemctl enable --now ssh
systemctl restart ssh
"#
}

fn restart_clone_daemon_script() -> &'static str {
    r#"set -e
runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user restart rmng-clone-daemon.service
"#
}

fn rmng_cli_shadow_cleanup_script() -> &'static str {
    r#"set -e
managed=/usr/local/bin/rmng
shadow=/home/rmng/.local/bin/rmng
test -x "$managed" || exit 0
resolved="$(runuser -u rmng -- bash -lc 'command -v rmng' 2>/dev/null || true)"
test "$resolved" = "$shadow" || exit 0
test -x "$shadow" || exit 0
managed_sha="$(sha256sum "$managed" | awk '{print $1}')"
shadow_sha="$(sha256sum "$shadow" | awk '{print $1}')"
test "$managed_sha" != "$shadow_sha" || exit 0
stamp="$(date +%Y%m%d%H%M%S)"
backup="${shadow}.shadowed-by-rmng-update.${stamp}"
i=0
while [ -e "$backup" ]; do
  i=$((i + 1))
  backup="${shadow}.shadowed-by-rmng-update.${stamp}.${i}"
done
mv -- "$shadow" "$backup"
echo "moved stale PATH-shadowing rmng CLI to $backup"
"#
}

fn tmp_mount_mask_script() -> &'static str {
    r#"set -e
systemctl mask tmp.mount >/dev/null 2>&1 || {
  mkdir -p /etc/systemd/system
  ln -sf /dev/null /etc/systemd/system/tmp.mount
}
systemctl daemon-reload >/dev/null 2>&1 || true
"#
}

fn etc_environment_sync_script(desired_env: &str) -> String {
    let desired_b64 = B64.encode(desired_env);
    format!(
        r#"set -e
etc=/etc/environment
legacy=/home/rmng/.config/environment.d/30-rmng-preset.conf
desired="$(mktemp)"
base="$(mktemp)"
tmp="$(mktemp)"
keys_file="$(mktemp)"
legacy_keys="$(mktemp)"
trap 'rm -f "$desired" "$base" "$tmp" "$keys_file" "$legacy_keys"' EXIT
base64 -d > "$desired" <<'RMNG_DESIRED_ENV'
{desired_b64}
RMNG_DESIRED_ENV
if [ -f "$etc" ]; then
  cp "$etc" "$base"
fi
if [ -f "$legacy" ]; then
  grep -E '^[A-Za-z_][A-Za-z0-9_]*=' "$legacy" | sed 's/=.*//' | sort -u > "$legacy_keys"
  awk -F= 'NR==FNR {{ drop[$1]=1; next }} !($1 in drop)' "$legacy_keys" "$base" > "$tmp"
  cat "$tmp" > "$base"
  awk '/^[A-Za-z_][A-Za-z0-9_]*=/' "$legacy" >> "$base"
fi
grep -E '^[A-Za-z_][A-Za-z0-9_]*=' "$desired" | sed 's/=.*//' | sort -u > "$keys_file"
awk -F= 'NR==FNR {{ drop[$1]=1; next }} !($1 in drop)' "$keys_file" "$base" > "$tmp"
if [ -s "$tmp" ] && [ "$(tail -c 1 "$tmp" | wc -l)" -eq 0 ]; then
  printf '\n' >> "$tmp"
fi
awk '/^[A-Za-z_][A-Za-z0-9_]*=/' "$desired" >> "$tmp"
rm -f "$legacy"
rmdir /home/rmng/.config/environment.d 2>/dev/null || true
if [ -s "$tmp" ] && [ "$(tail -c 1 "$tmp" | wc -l)" -eq 0 ]; then
  printf '\n' >> "$tmp"
fi
if [ -f "$etc" ] && cmp -s "$tmp" "$etc"; then
  exit 0
fi
install -m 0644 -o root -g root "$tmp" "$etc"
"#
    )
}

fn preset_for_host<'a>(cfg: &'a wire::AppConfig, host: &wire::Host) -> Option<&'a wire::Preset> {
    if let Some(name) = host.preset_name.as_deref().filter(|s| !s.trim().is_empty()) {
        if let Some(preset) = cfg.presets.iter().find(|p| p.name == name) {
            return Some(preset);
        }
    }
    if let Some(prefix) = host.linear_workspace.as_deref().filter(|s| !s.trim().is_empty()) {
        if let Some(preset) = crate::linear::pick_preset_by_prefix(&cfg.presets, prefix) {
            return Some(preset);
        }
        if let Some(preset) = cfg.presets.iter().find(|p| p.name.eq_ignore_ascii_case(prefix)) {
            return Some(preset);
        }
    }
    if let Some(label) = host.linear_label.as_deref().filter(|s| !s.trim().is_empty()) {
        if let Some(preset) = cfg
            .presets
            .iter()
            .find(|p| p.labels.iter().any(|candidate| candidate.eq_ignore_ascii_case(label)))
        {
            return Some(preset);
        }
    }
    None
}

async fn exec_ok(app: &App, clone_id: &str, script: &str, label: &str) -> Result<()> {
    let code = app
        .docker
        .exec_script(clone_id, script, &[], &[], |stream, line| {
            tracing::debug!(target: "clone_reconcile", "{clone_id} {label} {stream}: {line}");
        })
        .await
        .with_context(|| format!("{clone_id}: {label}"))?;
    if code != 0 {
        bail!("{clone_id}: {label} exited {code}");
    }
    Ok(())
}

async fn read_stamp(app: &App, clone_id: &str, path: &str, label: &str) -> Result<Option<String>> {
    let mut out = String::new();
    let script = format!("cat /{path} 2>/dev/null || true\n");
    let code = app
        .docker
        .exec_script(clone_id, &script, &[], &[], |stream, line| {
            if stream == "out" {
                out.push_str(line);
                out.push('\n');
            }
        })
        .await
        .with_context(|| format!("{clone_id}: reading {label} stamp"))?;
    if code != 0 {
        bail!("{clone_id}: reading {label} stamp exited {code}");
    }
    let stamp = out.trim();
    Ok((!stamp.is_empty()).then(|| stamp.to_string()))
}

async fn ensure_ssh_ready(app: &App, clone_id: &str) -> Result<()> {
    if read_stamp(app, clone_id, ssh_stamp_path(), "ssh")
        .await?
        .as_deref()
        == Some("ok")
    {
        return Ok(());
    }
    exec_ok(app, clone_id, ssh_prepare_script(), "prepare ssh dirs").await?;
    let entries = crate::ssh::clone_ssh_tar_entries(
        &app.config().data_dir,
        clone_id,
        &app.config().ssh.authorized_keys,
    )?;
    app.docker
        .upload_tar(clone_id, entries)
        .await
        .with_context(|| format!("{clone_id}: uploading ssh material"))?;
    exec_ok(app, clone_id, ssh_bootstrap_script(), "bootstrap sshd").await?;
    app.docker
        .upload_tar(clone_id, vec![ssh_stamp_entry()])
        .await
        .with_context(|| format!("{clone_id}: writing ssh stamp"))?;
    Ok(())
}

async fn control_mcp_url(app: &App) -> Option<String> {
    match app.docker.control_host().await {
        Ok(control) => Some(format!(
            "http://{control}:{}",
            app.config().listen.clone_mcp
        )),
        Err(e) => {
            tracing::warn!(
                target: "clone_reconcile",
                "could not resolve control-server host for Codex MCP config: {e}"
            );
            None
        }
    }
}

async fn ensure_codex_parity(app: &App, clone_id: &str) -> Result<bool> {
    let control_url = control_mcp_url(app).await;
    let cc_base = crate::provision::cc_base_url(app).await;
    let entries = codex_parity_entries(clone_id, control_url.as_deref(), cc_base.as_deref());
    let desired = desired_payload_hash(&entries);
    if read_stamp(app, clone_id, codex_parity_stamp_path(), "codex parity")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }

    exec_ok(app, clone_id, codex_prepare_script(), "prepare codex dirs").await?;
    app.docker
        .upload_tar(clone_id, entries)
        .await
        .with_context(|| format!("{clone_id}: uploading Codex parity config"))?;
    app.docker
        .upload_tar(clone_id, vec![codex_parity_stamp_entry(&desired)])
        .await
        .with_context(|| format!("{clone_id}: writing Codex parity stamp"))?;
    Ok(true)
}

/// One-time group-proxy migration: remove the dead provider credential files from an existing
/// clone (see [`dead_creds_cleanup_script`]). Stamped so it runs once; best-effort at the call
/// site. Returns whether the cleanup ran this pass.
async fn ensure_dead_creds_removed(app: &App, clone_id: &str) -> Result<bool> {
    if read_stamp(app, clone_id, dead_creds_stamp_path(), "group-proxy migration")
        .await?
        .as_deref()
        == Some("ok")
    {
        return Ok(false);
    }
    exec_ok(app, clone_id, dead_creds_cleanup_script(), "remove dead provider credentials").await?;
    app.docker
        .upload_tar(
            clone_id,
            vec![TarEntry {
                path: dead_creds_stamp_path().to_string(),
                data: b"ok\n".to_vec(),
                mode: 0o644,
                uid: 0,
                gid: 0,
            }],
        )
        .await
        .with_context(|| format!("{clone_id}: writing group-proxy migration stamp"))?;
    Ok(true)
}

async fn ensure_codex_cli(app: &App, clone_id: &str) -> Result<()> {
    let code = app
        .docker
        .exec_script(
            clone_id,
            codex_cli_install_script(),
            &[],
            &[],
            |stream, line| {
                tracing::debug!(target: "clone_reconcile", "{clone_id} codex cli {stream}: {line}");
            },
        )
        .await
        .with_context(|| format!("{clone_id}: ensuring Codex CLI"))?;
    if code != 0 {
        bail!("{clone_id}: Codex CLI install exited {code}");
    }
    Ok(())
}

async fn ensure_payload_current(app: &App, clone_id: &str) -> Result<bool> {
    let entries = binary_payload_entries()?;
    let desired = desired_payload_hash(&entries);
    if read_stamp(app, clone_id, payload_stamp_path(), "payload")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        exec_ok(
            app,
            clone_id,
            rmng_cli_shadow_cleanup_script(),
            "clean stale rmng CLI shadow",
        )
        .await?;
        return Ok(false);
    }

    app.docker
        .upload_tar(clone_id, entries)
        .await
        .with_context(|| format!("{clone_id}: uploading clone binaries"))?;
    exec_ok(
        app,
        clone_id,
        restart_clone_daemon_script(),
        "restart rmng-clone-daemon",
    )
    .await?;
    app.docker
        .upload_tar(clone_id, vec![payload_stamp_entry(&desired)])
        .await
        .with_context(|| format!("{clone_id}: writing payload stamp"))?;
    exec_ok(
        app,
        clone_id,
        rmng_cli_shadow_cleanup_script(),
        "clean stale rmng CLI shadow",
    )
    .await?;
    Ok(true)
}

async fn reconcile_once(app: &App, warned: &mut HashSet<String>) {
    let hosts: Vec<_> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && is_safe_id(&h.id))
        .collect();

    let cfg = app.config();
    let control_env = crate::provision::control_env_vars(app).await;

    for h in &hosts {
        let id = h.id.as_str();
        if !app.docker.is_running(id).await.unwrap_or(false) {
            continue;
        }
        match ensure_ssh_ready(app, id).await {
            Ok(()) => {}
            Err(e) => {
                if warned.insert(format!("{id}:ssh")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: ssh reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: ssh reconcile still failing: {e:#}");
                }
                continue;
            }
        }
        warned.remove(&format!("{id}:ssh"));

        let mut desired_env = control_env.clone();
        // Per-clone group-proxy router key (ANTHROPIC_AUTH_TOKEN / RMNG_PROXY_KEY): recomputed
        // into `/etc/environment` on every resync so an existing clone (created before the
        // group-proxy model) picks it up without a recreate. Minted + persisted server-side;
        // never serialized onto `Host`/state.
        desired_env.extend(crate::provision::router_env_vars(app, id));
        if let Some(preset) = preset_for_host(&cfg, h) {
            desired_env.extend(crate::provision::preset_env_vars(preset));
        } else if h.preset_name.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            tracing::warn!(
                target: "clone_reconcile",
                "clone {id}: preset {:?} no longer exists; preserving unmanaged /etc/environment keys",
                h.preset_name
            );
        }
        let desired_env = crate::provision::clone_etc_environment_conf(&desired_env);
        let env_script = etc_environment_sync_script(&desired_env);
        match exec_ok(
            app,
            id,
            &env_script,
            "sync /etc/environment",
        )
        .await
        {
            Ok(()) => {
                warned.remove(&format!("{id}:etc-env"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:etc-env")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: /etc/environment reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: /etc/environment reconcile still failing: {e:#}");
                }
            }
        }

        match exec_ok(app, id, tmp_mount_mask_script(), "mask tmp.mount").await {
            Ok(()) => {
                warned.remove(&format!("{id}:tmp-mount"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:tmp-mount")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: tmp.mount reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: tmp.mount reconcile still failing: {e:#}");
                }
            }
        }

        match ensure_codex_cli(app, id).await {
            Ok(()) => {
                warned.remove(&format!("{id}:codex-cli"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:codex-cli")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: Codex CLI reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: Codex CLI reconcile still failing: {e:#}");
                }
            }
        }

        match ensure_codex_parity(app, id).await {
            Ok(true) => {
                warned.remove(&format!("{id}:codex"));
                tracing::info!(
                    target: "clone_reconcile",
                    "clone {id}: refreshed Codex AGENTS.md and MCP config"
                );
            }
            Ok(false) => {
                warned.remove(&format!("{id}:codex"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:codex")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: Codex parity reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: Codex parity reconcile still failing: {e:#}");
                }
                continue;
            }
        }

        // Group-proxy migration: strip the now-dead provider credential files so an existing
        // clone stops using its own tokens and routes through the `/cc` proxy instead (its env
        // + agent configs were rewritten above). Best-effort + stamped — a failure is logged
        // and retried next pass, never fatal to the rest of the reconcile.
        match ensure_dead_creds_removed(app, id).await {
            Ok(true) => {
                warned.remove(&format!("{id}:creds-migrate"));
                tracing::info!(target: "clone_reconcile", "clone {id}: removed dead provider credentials (group-proxy migration)");
            }
            Ok(false) => {
                warned.remove(&format!("{id}:creds-migrate"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:creds-migrate")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: group-proxy credential migration failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: group-proxy credential migration still failing: {e:#}");
                }
            }
        }

        match ensure_payload_current(app, id).await {
            Ok(true) => {
                warned.remove(&format!("{id}:payload"));
                tracing::info!(target: "clone_reconcile", "clone {id}: refreshed clone binaries and restarted rmng-clone-daemon");
            }
            Ok(false) => {
                warned.remove(&format!("{id}:payload"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:payload")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: payload reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: payload reconcile still failing: {e:#}");
                }
            }
        }
    }

    let managed: HashSet<String> = hosts.iter().map(|h| h.id.clone()).collect();
    warned.retain(|key| {
        key.split_once(':')
            .map(|(id, _)| managed.contains(id))
            .unwrap_or(false)
    });
}

pub async fn run(app: App) {
    tracing::info!(
        "clone reconciler started (ssh + Codex config + binary refresh, every {}s)",
        RECONCILE_INTERVAL.as_secs()
    );
    let mut warned = HashSet::new();
    loop {
        reconcile_once(&app, &mut warned).await;
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_stamp_path_is_under_opt_rmng() {
        assert_eq!(payload_stamp_path(), "opt/rmng/.payload-hash");
    }

    #[test]
    fn ssh_stamp_path_is_under_etc_rmng() {
        assert_eq!(ssh_stamp_path(), "etc/rmng/ssh-ready");
    }

    #[test]
    fn ssh_stamp_entry_marks_success_with_root_owned_file() {
        let entry = ssh_stamp_entry();
        assert_eq!(entry.path, "etc/rmng/ssh-ready");
        assert_eq!(entry.data, b"ok\n");
        assert_eq!(entry.mode, 0o644);
        assert_eq!((entry.uid, entry.gid), (0, 0));
    }

    #[test]
    fn codex_parity_entries_install_global_guidance_and_linear_mcp() {
        let entries = codex_parity_entries("rmng-a", Some("http://rmng-control:9002"), None);
        let agents = entries
            .iter()
            .find(|e| e.path == "home/rmng/.codex/AGENTS.md")
            .expect("missing Codex AGENTS.md");
        assert_eq!(agents.mode, 0o644);
        assert_eq!((agents.uid, agents.gid), (1000, 1000));
        let agents_body = String::from_utf8(agents.data.clone()).unwrap();
        assert!(agents_body.contains("disposable, single-purpose dev sandbox"));
        assert!(agents_body.contains("passwordless `sudo`"));

        let config = entries
            .iter()
            .find(|e| e.path == "home/rmng/.codex/config.toml")
            .expect("missing Codex config.toml");
        assert_eq!(config.mode, 0o600);
        assert_eq!((config.uid, config.gid), (1000, 1000));
        let config_body = String::from_utf8(config.data.clone()).unwrap();
        assert!(config_body.contains("[mcp_servers.desktop]"));
        assert!(config_body.contains("url = \"http://127.0.0.1:9004\""));
        assert!(config_body.contains("[mcp_servers.linear]"));
        assert!(config_body.contains("url = \"https://mcp.linear.app/mcp\""));
        assert!(config_body.contains("bearer_token_env_var = \"LINEAR_API_KEY\""));
        assert!(config_body.contains("[mcp_servers.\"control-server\"]"));
        assert!(config_body.contains("url = \"http://rmng-control:9002\""));
        assert!(config_body.contains("\"x-rmng-clone\" = \"rmng-a\""));
    }

    #[test]
    fn codex_config_adds_active_rmng_provider_when_cc_base_present() {
        let toml = codex_config_toml("rmng-a", None, Some("http://rmng-control:9000/cc/v1"));
        assert!(toml.contains("model_provider = \"rmng\""));
        assert!(toml.contains("[model_providers.rmng]"));
        assert!(toml.contains("base_url = \"http://rmng-control:9000/cc/v1\""));
        assert!(toml.contains("wire_api = \"responses\""));
        assert!(toml.contains("env_key = \"RMNG_PROXY_KEY\""));
        // HTTP+SSE only: the Responses-API WebSocket transport is explicitly disabled.
        assert!(toml.contains("supports_websockets = false"));
        // Default model is a GPT one so Claude models can't be picked from Codex.
        assert!(toml.contains(&format!("model = \"{}\"", RMNG_GPT_MODELS[0])));
        assert!(RMNG_GPT_MODELS[0].starts_with("gpt-"));
        // Bare top-level keys must precede the first [table] (valid TOML).
        let mp = toml.find("model_provider = ").unwrap();
        let first_table = toml.find("[mcp_servers.desktop]").unwrap();
        assert!(mp < first_table, "top-level keys must come before tables:\n{toml}");
        // No cc base → the old behavior (no rmng provider at all).
        let plain = codex_config_toml("rmng-a", None, None);
        assert!(!plain.contains("model_providers.rmng"));
        assert!(!plain.contains("model_provider"));
    }

    #[test]
    fn opencode_config_is_gpt_only_openai_compatible_provider() {
        assert!(opencode_config_json(None).is_none());
        let json = opencode_config_json(Some("http://rmng-control:9000/cc/v1")).unwrap();
        assert!(json.contains("\"npm\": \"@ai-sdk/openai-compatible\""));
        assert!(json.contains("\"baseURL\": \"http://rmng-control:9000/cc/v1\""));
        assert!(json.contains("{env:RMNG_PROXY_KEY}"));
        assert!(json.contains(RMNG_GPT_MODELS[0]));
        // Default model is set as "<provider>/<id>" pointing at the GPT default.
        assert!(json.contains(&format!("\"model\": \"rmng/{}\"", RMNG_GPT_MODELS[0])));
        // No Anthropic/Claude provider is generated for OpenCode.
        let lower = json.to_lowercase();
        assert!(!lower.contains("anthropic"));
        assert!(!lower.contains("claude"));
        // The parity entries carry the opencode file when a cc base is present.
        let entries = codex_parity_entries("rmng-a", None, Some("http://rmng-control:9000/cc/v1"));
        assert!(entries.iter().any(|e| e.path == "home/rmng/.config/opencode/opencode.json"));
        // ...and omit it when there's no cc base.
        let bare = codex_parity_entries("rmng-a", None, None);
        assert!(!bare.iter().any(|e| e.path == "home/rmng/.config/opencode/opencode.json"));
    }

    #[test]
    fn codex_parity_stamp_hash_changes_when_config_changes() {
        let original = codex_parity_stamp_entry_for(&codex_parity_entries("rmng-a", None, None));
        let mut changed = codex_parity_entries("rmng-a", None, None);
        changed
            .iter_mut()
            .find(|e| e.path == "home/rmng/.codex/config.toml")
            .unwrap()
            .data
            .extend_from_slice(b"\n# changed\n");
        let updated = codex_parity_stamp_entry_for(&changed);

        assert_eq!(original.path, "etc/rmng/codex-parity-hash");
        assert_eq!(updated.path, "etc/rmng/codex-parity-hash");
        assert_ne!(original.data, updated.data);
    }

    #[test]
    fn codex_prepare_script_best_effort_installs_missing_cli() {
        let script = codex_cli_install_script();
        assert!(script.contains("command -v codex"));
        assert!(script.contains("CODEX_NON_INTERACTIVE=1"));
        assert!(script.contains("https://chatgpt.com/codex/install.sh"));
        assert!(script.contains("codex install failed"));
    }

    #[test]
    fn rmng_cli_shadow_cleanup_moves_only_stale_user_local_binary() {
        let script = rmng_cli_shadow_cleanup_script();
        assert!(script.contains("command -v rmng"));
        assert!(script.contains("/home/rmng/.local/bin/rmng"));
        assert!(script.contains("/usr/local/bin/rmng"));
        assert!(script.contains("sha256sum"));
        assert!(script.contains("mv -- \"$shadow\""));
        assert!(script.contains(".shadowed-by-rmng-update."));
    }

    #[test]
    fn tmp_mount_mask_script_disables_future_tmpfs_without_unmounting_live_tmp() {
        let script = tmp_mount_mask_script();
        assert!(script.contains("systemctl mask tmp.mount"));
        assert!(script.contains("/etc/systemd/system/tmp.mount"));
        assert!(script.contains("daemon-reload"));
        assert!(!script.contains("systemctl stop tmp.mount"));
        assert!(!script.contains("umount"));
    }

    #[test]
    fn etc_environment_sync_uses_desired_env_and_removes_legacy_environment_d() {
        let script = etc_environment_sync_script("RMNG_CONTROL_URL=http://rmng-control:9000\nLINEAR_API_KEY=secret\n");
        assert!(script.contains("base64 -d"));
        assert!(script.contains("/etc/environment"));
        assert!(script.contains("drop[$1]=1"));
        assert!(script.contains("awk '/^[A-Za-z_][A-Za-z0-9_]*=/' \"$desired\" >> \"$tmp\""));
        assert!(script.contains("cmp -s \"$tmp\" \"$etc\""));
        assert!(script.contains("install -m 0644"));
        assert!(script.contains("rm -f \"$legacy\""));
    }

    #[test]
    fn desired_payload_hash_changes_when_payload_bytes_change() {
        let one = desired_payload_hash(&[TarEntry {
            path: "opt/rmng/bin/rmng-clone-daemon".into(),
            data: b"old".to_vec(),
            mode: 0o755,
            uid: 0,
            gid: 0,
        }]);
        let two = desired_payload_hash(&[TarEntry {
            path: "opt/rmng/bin/rmng-clone-daemon".into(),
            data: b"new".to_vec(),
            mode: 0o755,
            uid: 0,
            gid: 0,
        }]);
        assert_ne!(one, two);
    }

    #[test]
    fn desired_payload_hash_changes_when_install_path_changes() {
        let one = desired_payload_hash(&[TarEntry {
            path: "opt/rmng/bin/agent-wrapper".into(),
            data: b"same".to_vec(),
            mode: 0o755,
            uid: 0,
            gid: 0,
        }]);
        let two = desired_payload_hash(&[TarEntry {
            path: "usr/local/bin/rmng".into(),
            data: b"same".to_vec(),
            mode: 0o755,
            uid: 0,
            gid: 0,
        }]);
        assert_ne!(one, two);
    }

    #[test]
    fn ssh_bootstrap_script_installs_and_enables_pubkey_only_sshd() {
        let script = ssh_bootstrap_script();
        for needle in [
            "apt-get install",
            "openssh-server",
            "/home/rmng/.ssh",
            "PasswordAuthentication no",
            "PermitRootLogin no",
            "AllowUsers rmng",
            "mkdir -p /etc/rmng",
            "systemctl enable --now ssh",
        ] {
            assert!(
                script.contains(needle),
                "bootstrap script missing `{needle}`:\n{script}"
            );
        }
    }
}
