//! Live migration for clones created by older control-server/template versions.
//!
//! New clones get current binaries and SSH material during `provision::clone_container`.
//! Existing running clones need an idempotent reconcile path so a control-server update can
//! make them operational without destructive recreate: install/enable clone-side sshd, refresh
//! injected payload binaries, then restart the clone daemon and agent wrapper so their running
//! processes use the current payload and configuration.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use crate::app::App;
use crate::docker::TarEntry;
use crate::files::is_safe_id;

const CLONE_UID: u64 = 1000;
const CLONE_GID: u64 = 1000;

// ---- managed MCP servers: the single source of truth ---------------------------------
//
// The `desktop` + `linear` set every clone agent gets, defined ONCE here and rendered into
// each agent's own format by the emitters below (Claude `~/.claude.json` merge, Codex
// `config.toml` merge, Cursor `~/.cursor/mcp.json` merge, and the neutral `~/.config/rmng/mcp.json`
// the node-agent reads). Change a URL / add a server here and all agents pick it up.

/// One managed MCP server. All fields are static — the list is compile-time constant.
#[derive(Clone, Copy)]
struct ManagedMcp {
    /// Server key (e.g. `desktop`, `linear`). Also the jq / TOML table / JSON map key.
    name: &'static str,
    url: &'static str,
    /// Omit on headless clones — the `desktop` computer-use daemon (:9004) only exists on
    /// headed clones, so pointing an agent at it there would be a dead endpoint.
    headless_only: bool,
    /// `Some(env)` ⇒ authenticate with `Authorization: Bearer <$env>`, resolved from the clone
    /// env at runtime (each emitter renders the env reference in its own syntax).
    bearer_env: Option<&'static str>,
    /// node-agent (Claude Agent SDK) hint: keep this server's tools in context every turn.
    /// Ignored by the file-based agents (Claude CLI / Codex).
    always_load: bool,
}

/// THE managed MCP set. Order is stable (used verbatim by the emitters).
fn managed_mcp() -> [ManagedMcp; 2] {
    [
        ManagedMcp {
            name: "desktop",
            url: "http://127.0.0.1:9004",
            headless_only: true,
            bearer_env: None,
            always_load: true,
        },
        ManagedMcp {
            name: "linear",
            url: "https://mcp.linear.app/mcp",
            headless_only: false,
            bearer_env: Some("LINEAR_API_KEY"),
            always_load: false,
        },
    ]
}

/// The managed servers active on a clone of the given headless-ness.
fn active_mcp(headless: bool) -> Vec<ManagedMcp> {
    managed_mcp()
        .into_iter()
        .filter(|m| !(headless && m.headless_only))
        .collect()
}

/// Codex `[mcp_servers.*]` tables (config.toml). linear auths via `bearer_token_env_var`.
/// `pub(crate)`: the create path renders the fresh-clone initial file from this directly.
pub(crate) fn codex_mcp_toml(headless: bool) -> String {
    let mut s = String::new();
    for m in active_mcp(headless) {
        s.push_str(&format!("[mcp_servers.{}]\nurl = \"{}\"\n", m.name, m.url));
        if let Some(env) = m.bearer_env {
            s.push_str(&format!("bearer_token_env_var = \"{env}\"\n"));
        }
        s.push('\n');
    }
    s
}

/// Merge the managed MCP set into a `~/.claude.json` body: set each active server,
/// skip each inactive one. Set-only by design — headless is immutable per clone, so a
/// headless clone never grows `desktop` in the first place and there is nothing to
/// delete. Same shapes (linear's bearer stays the literal `${LINEAR_API_KEY}`, which
/// Claude Code expands from the session env at runtime). A non-object base is a hard
/// error; a null/missing `mcpServers` seeds empty.
/// Set one top-level key on a JSON object body, keeping everything else. The shared
/// shape behind all four agent-file merges (Claude/Cursor MCP sets, both hook
/// registrations): per-file code builds the value, this plants it. A non-object base is
/// a hard error naming the file.
fn set_json_key(
    base: &serde_json::Value,
    path: &str,
    key: &str,
    value: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let mut root = base
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{path} is not a JSON object"))?;
    root.insert(key.to_string(), value);
    Ok(serde_json::Value::Object(root))
}

pub(crate) fn merge_claude_mcp(
    base: &serde_json::Value,
    headless: bool,
) -> anyhow::Result<serde_json::Value> {
    let mut servers = base
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for m in managed_mcp() {
        // Inactive servers are skipped, never deleted: nothing writes them, so a
        // headless file simply never contains `desktop`.
        if headless && m.headless_only {
            continue;
        }
        let mut obj = serde_json::json!({ "type": "http", "url": m.url });
        if let Some(env) = m.bearer_env {
            obj["headers"] =
                serde_json::json!({ "Authorization": format!("Bearer ${{{env}}}") });
        }
        servers.insert(m.name.to_string(), obj);
    }
    set_json_key(
        base,
        "~/.claude.json",
        "mcpServers",
        serde_json::Value::Object(servers),
    )
}

/// `{name,url,bearerEnv?,alwaysLoad?}`. The agent-wrapper maps this to the Claude Agent SDK's
/// `mcpServers` (resolving `bearerEnv` from `process.env`, skipping a server whose bearer env is
/// empty). Headless-filtered here so the wrapper needs no headless logic of its own.
fn mcp_descriptor_json(headless: bool) -> String {
    let servers: Vec<serde_json::Value> = active_mcp(headless)
        .into_iter()
        .map(|m| {
            let mut o = serde_json::json!({ "name": m.name, "url": m.url });
            if let Some(env) = m.bearer_env {
                o["bearerEnv"] = serde_json::json!(env);
            }
            if m.always_load {
                o["alwaysLoad"] = serde_json::json!(true);
            }
            o
        })
        .collect();
    // Infallible in practice: the input is the static managed set, which always serializes.
    // The expect (not a silent `"[]"`) keeps a serialization regression loud — an empty
    // array here would silently strip every managed server from every clone.
    serde_json::to_string_pretty(&serde_json::json!(servers))
        .expect("static managed MCP set serializes")
}

/// The `mcpServers` entries Cursor should hold.
///
/// Cursor recognizes `command`/`args`/`env`/`url`/`headers`/`auth` per server and derives the
/// transport itself (a `url` server becomes `streamableHttp`), so no `type` is written. It does
/// **not** expand environment references anywhere in this file, which is why the bearer is
/// resolved here instead of being left as `${LINEAR_API_KEY}` the way Claude Code takes it. A
/// server the clone cannot authenticate is skipped rather than written headerless: Cursor shows
/// a broken server as "Needs attention" until someone clears it. Set-only like the other
/// merges — skipped servers are never written, never deleted.
fn cursor_mcp_want(headless: bool, linear_key: &str) -> serde_json::Value {
    let mut want = serde_json::Map::new();
    for m in managed_mcp() {
        let bearer = match m.bearer_env {
            Some(_) if linear_key.is_empty() => continue,
            Some(_) => Some(format!("Bearer {linear_key}")),
            None => None,
        };
        if headless && m.headless_only {
            continue;
        }
        let mut server = serde_json::json!({ "url": m.url });
        if let Some(b) = bearer {
            server["headers"] = serde_json::json!({ "Authorization": b });
        }
        want.insert(m.name.to_string(), server);
    }
    serde_json::Value::Object(want)
}

/// Merge the managed MCP set into a `~/.cursor/mcp.json` body: set each wanted server
/// under `.mcpServers`, skip the rest. Other top-level keys and the operator's own
/// servers survive. A non-object base is a hard error.
pub(crate) fn merge_cursor_mcp(
    base: &serde_json::Value,
    headless: bool,
    linear_key: &str,
) -> anyhow::Result<serde_json::Value> {
    let want = cursor_mcp_want(headless, linear_key);
    let mut servers = match base.get("mcpServers") {
        None | Some(serde_json::Value::Null) => serde_json::Map::new(),
        Some(serde_json::Value::Object(map)) => map.clone(),
        Some(_) => {
            return Err(anyhow::anyhow!(
                "~/.cursor/mcp.json .mcpServers is not an object"
            ));
        }
    };
    for (name, server) in want.as_object().cloned().unwrap_or_default() {
        servers.insert(name, server);
    }
    set_json_key(
        base,
        "~/.cursor/mcp.json",
        "mcpServers",
        serde_json::Value::Object(servers),
    )
}

fn cursor_mcp_stamp_path() -> &'static str {
    "etc/rmng/cursor-mcp"
}

/// Hash of the script itself, so the headless bit, a rotated Linear key, and any future change
/// to the managed set all re-apply on the next pass. The key is never in the stamp.
/// Hash of the canonical merge output (Bearer key resolved), so the headless bit, a
/// rotated Linear key, and any future change to the managed set all re-apply on the next
/// pass. Only the hash is stored — the key itself never lands in a stamp file.
fn cursor_mcp_desired(headless: bool, linear_key: &str) -> String {
    // `expect`: merging onto `{}` cannot fail (only a non-object base errors).
    let canonical = merge_cursor_mcp(&serde_json::json!({}), headless, linear_key)
        .expect("empty object takes the Cursor MCP merge")
        .to_string();
    desired_payload_hash(&[TarEntry {
        path: "cursor-mcp".into(),
        data: canonical.into_bytes(),
        mode: 0,
        uid: 0,
        gid: 0,
    }])
}

pub(crate) fn cursor_mcp_stamp_entry_for(headless: bool, linear_key: &str) -> TarEntry {
    TarEntry {
        path: cursor_mcp_stamp_path().to_string(),
        data: format!("{}\n", cursor_mcp_desired(headless, linear_key)).into_bytes(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

/// The value `/etc/environment` will end up with for `key`, or `""`. Last duplicate wins, the
/// same precedence [`crate::provision::etc_environment_conf`] applies when it writes the file.
pub(crate) fn env_value(vars: &[wire::EnvVar], key: &str) -> String {
    vars.iter()
        .rev()
        .find(|v| v.key == key)
        .map(|v| v.value.clone())
        .unwrap_or_default()
}

fn payload_stamp_path() -> &'static str {
    "opt/rmng/.payload-hash"
}

fn ssh_stamp_path() -> &'static str {
    "etc/rmng/ssh-ready"
}

fn codex_parity_stamp_path() -> &'static str {
    "etc/rmng/codex-parity-hash"
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

fn binary_payload_entries(headless: bool) -> Result<Vec<TarEntry>> {
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
    // A headless clone has no desktop for the holder to hold: `provision.rs` deletes the
    // gnome-headless and clone-daemon units there, and a holder unit would restart-loop
    // against a Mutter that is never coming up.
    if !headless {
        entries.push(session_holder_unit_entry());
    }
    Ok(entries)
}

/// The `rmng-session-holder.service` unit, shipped with the binaries so a clone created
/// before the holder existed picks it up without being recreated.
///
/// It carries no `RMNG_MONITORS`. The holder remembers the last layout it applied in
/// `~/.rmng/monitors` and boots on that, which is current in a way a value baked into an
/// image never is. Only a clone that has never run a holder falls back to the unit's
/// environment, and there the control-server's push a second later is the correction.
pub(crate) fn session_holder_unit_entry() -> TarEntry {
    TarEntry {
        path: "home/rmng/.config/systemd/user/rmng-session-holder.service".to_string(),
        data: SESSION_HOLDER_UNIT.as_bytes().to_vec(),
        mode: 0o644,
        uid: CLONE_UID,
        gid: CLONE_GID,
    }
}

const SESSION_HOLDER_UNIT: &str = "\
[Unit]
Description=rmng session holder (Mutter session + virtual monitors)
After=gnome-headless.service
Wants=gnome-headless.service
[Service]
Type=simple
Environment=WAYLAND_DISPLAY=wayland-0
ExecStart=/opt/rmng/bin/rmng-clone-daemon --session-holder
Restart=on-failure
RestartSec=2
[Install]
WantedBy=default.target
";

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

/// Current SSH-provisioning schema version. History: `ok` originally; `v2` when the shared fleet
/// key moved out of `~/.ssh`; `v3` now that the fleet key and the managed `~/.ssh/config` block
/// are gone entirely and `authorized_keys` is the only file provisioned.
///
/// The bump matters for `v2` clones specifically: they were provisioned with the fleet pubkey
/// folded into their `authorized_keys`, so re-running `ensure_ssh_ready` is what rewrites that
/// file WITHOUT it — otherwise every clone would keep accepting the retired fleet identity
/// indefinitely. Their now-orphaned `~/.ssh/config` and any leftover key files are left alone
/// on purpose: they are the user's to keep or delete.
const SSH_STAMP_VERSION: &str = "v3";

pub(crate) fn ssh_stamp_entry() -> TarEntry {
    TarEntry {
        path: ssh_stamp_path().to_string(),
        data: format!("{SSH_STAMP_VERSION}\n").into_bytes(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

/// Claude Code's default model — `opus[1m]`, its `opus` alias with the 1M-context beta.
///
/// A client-side alias Claude Code resolves against the models its endpoint serves, so it tracks
/// the flagship without pinning a version id here. It used to be resolved per-group from the
/// CLIProxyAPI instance's live `/v1/models` catalog; with the group proxy gone every clone talks
/// to Anthropic directly, so there is no catalog to consult and this constant IS the answer.
const FALLBACK_CLAUDE_MODEL: &str = "opus[1m]";

/// A clone's `ANTHROPIC_MODEL` line.
///
/// Shared by BOTH env-writing paths so they agree byte-for-byte: the create path
/// (`jobs::run_clone`) and the trigger-driven resync. Keeping one definition is what stops a
/// fresh clone from being born without the var — a visible window in which the clone's
/// Claude Code ran on its built-in default instead of ours.
pub(crate) fn claude_model_env_var() -> wire::EnvVar {
    wire::EnvVar {
        key: "ANTHROPIC_MODEL".into(),
        value: FALLBACK_CLAUDE_MODEL.to_string(),
    }
}

/// Merge the managed MCP tables into a clone's `~/.codex/config.toml`, preserving everything
/// else in the file.
///
/// This used to overwrite the file wholesale. It must not: `config.toml` is where a Codex user
/// puts their own settings (`model`, `approval_policy`, `sandbox_mode`, their own
/// `[mcp_servers.*]`), and a rewrite every reconcile pass reverted any hand-edit within ~30 s
/// with no warning. `~/.claude.json` already got the careful treatment (a jq merge, because it
/// is state-bearing); this is the same courtesy for the file Codex owns.
///
/// Managed tables RMNG used to write here and no longer does (the group-proxy era's
/// `[model_providers.rmng]` + bare `model_provider`/`model` keys) pass through like any
/// other operator content now: the merge is set-only and there are no stale clones to
/// heal. Note the trade this accepts: a `model_provider` key beats the `~/.codex/auth.json`
/// the server writes, so a clone that still carries the old wiring stays broken until its
/// owner clears those lines. `model_reasoning_effort` was never touched and still isn't —
/// a plain preference, not wiring.
/// Merge the managed MCP tables into a `~/.codex/config.toml` body: for each managed
/// table, replace its block in place when present, append it when missing. Every other
/// line passes through verbatim except the blank run before a table header, which is
/// normalized to exactly one (so replacement and appends share one layout, and a
/// converged file merges to itself). Set-only by design like the other merges: tables
/// no longer emitted
/// (headless `desktop`) and tables from older servers are left alone, never deleted.
/// Replacing in place (rather than drop-then-append) keeps the merge idempotent without
/// any deletion: a converged file merges to itself byte-for-byte.
pub(crate) fn merge_codex_config(current: &str, headless: bool) -> String {
    // Desired table name → block lines (no trailing blank; separators are added below).
    let mut want: Vec<(String, Vec<String>)> = Vec::new();
    for (name, lines) in split_toml_tables(&codex_mcp_toml(headless)) {
        if let Some(n) = name {
            want.push((n, lines.into_iter().map(str::to_string).collect()));
        }
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    // True once a managed block was the last thing emitted: the file then ends with
    // exactly one blank line (the `codex_mcp_toml` shape). Passthrough lines clear it.
    let mut ends_managed = false;
    // Every table header — replaced or passed through — is preceded by exactly one
    // blank line (unless at start of file). All other lines pass through verbatim.
    let emit_header_separator = |out: &mut Vec<String>| {
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        if !out.is_empty() {
            out.push(String::new());
        }
    };
    let mut skip = false;
    for line in current.lines() {
        if let Some(name) = toml_table_header(line) {
            match want.iter().find(|(n, _)| *n == name) {
                Some((_, block)) => {
                    emit_header_separator(&mut out);
                    out.extend(block.iter().cloned());
                    seen.insert(name);
                    skip = true;
                    ends_managed = true;
                }
                None => {
                    emit_header_separator(&mut out);
                    skip = false;
                    out.push(line.to_string());
                    ends_managed = false;
                }
            }
            continue;
        }
        if !skip {
            out.push(line.to_string());
            ends_managed = false;
        }
    }
    let missing: Vec<&Vec<String>> = want
        .iter()
        .filter(|(n, _)| !seen.contains(n))
        .map(|(_, b)| b)
        .collect();
    if !missing.is_empty() {
        for (i, block) in missing.iter().enumerate() {
            if i > 0 || !out.iter().all(|l| l.trim().is_empty()) {
                emit_header_separator(&mut out);
            } else {
                out.clear();
            }
            out.extend(block.iter().cloned());
        }
        ends_managed = true;
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    if ends_managed && !out.is_empty() {
        out.push(String::new());
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Split TOML text into `(table name or None for the preface, lines)` chunks. Line-based
/// like the old awk pass: a `[table]` header starts a chunk, everything else accumulates.
fn split_toml_tables(text: &str) -> Vec<(Option<String>, Vec<&str>)> {
    let mut chunks = vec![];
    let mut name: Option<String> = None;
    let mut lines: Vec<&str> = vec![];
    for line in text.lines() {
        if let Some(header) = toml_table_header(line) {
            chunks.push((name.take(), std::mem::take(&mut lines)));
            name = Some(header);
        }
        lines.push(line);
    }
    chunks.push((name, lines));
    chunks
}

/// A TOML `[table]` header's dotted name, if the line is one. Inner whitespace is trimmed
/// (TOML allows `[ table ]`); anything else passes through as non-header.
fn toml_table_header(line: &str) -> Option<String> {
    let t = line.trim();
    if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
        Some(t[1..t.len() - 1].trim().to_string())
    } else {
        None
    }
}


const RMNG_CLI_SKILL_MD: &str = r#"---
name: rmng-cli
description: "Use when you need to manage the RMNG clone fleet from inside a clone: list clones, create or destroy clones, open an SSH/exec session into another clone, drive a clone's desktop, manage clone-source images and agent accounts, or search what other clones have already worked through in their own transcripts. Covers the `rmng` command-line tool."
---

# Managing the fleet with `rmng`

`rmng` is the RMNG fleet CLI, pre-installed at `/usr/local/bin/rmng` in every clone. Inside a
clone it auto-resolves the control-server (via `$RMNG_CONTROL_URL`), so commands work with no
setup. It talks to the control-server's web API — it does NOT need Docker or root.

The surface is **noun → verb**: `rmng <noun> <verb> [<clone>] [flags]`. Every command takes
`--json` for machine-readable output (tables/prose go to stdout, progress/prompts to stderr;
under `--json` even errors are JSON). The target is always the **clone id** — the first column
of `rmng clone ls`.

## Headed vs headless clones

Every clone is one of two kinds, fixed at creation:
- **headed** (the default) — a full GUI desktop. Supports computer use via `rmng desktop`
  (screenshot/click/type/…) and a video stream in the viewer. Heavier.
- **headless** (`rmng clone create … --headless`) — no desktop; a terminal/tmux view only.
  Lighter and faster to boot. `rmng desktop` does NOT work on a headless clone (it has no
  desktop MCP) — use `rmng clone exec` / `rmng clone ssh` instead.

Pick **headless** for pure coding/CLI work; pick **headed** only when the task needs a browser
or GUI. The kind can't be changed after creation.

## Inspect the fleet

- `rmng clone ls` — list clones with live CPU, RAM, status, the board column each sits in, and
  each provider's bound account. Sub clones are indented under their parent. `--json` gives one
  object per clone with `stats` nested.
- `rmng op ls` — list recent operations (clone / delete / archive / restore / pull / commit /
  update).
- `rmng op wait <op-id> [--timeout <secs>]` — block until an operation reaches a terminal state.

## The board

The dashboard arranges clones in columns, and the CLI reads and writes the same board.

- `rmng board ls` — the columns left to right, with what is in each. A clone nobody filed is
  still reported in the column the board draws it in.
- `rmng board move <clone> "<column>"` — put a clone at the **top** of a column. Name it the
  way it reads on the board (`"In Progress"`); the stored id works too, and case and spacing
  do not matter.
- Every create verb takes `--column "<name>"`, which files the new clone at the top of it.

**Moving into an archive column archives the clone**, and moving it back out restores it,
exactly as dropping a card there does on the dashboard. Add `--wait` to block on that.
A sub clone cannot be filed: it is drawn under its parent's card, so move the parent.

## Reach another clone

- `rmng clone ssh <clone>` — print a ready-to-paste `ssh` command for a clone.
- `rmng clone self` — this clone's own record (its id, image, address and accounts).
- `rmng clone fork <source> <new-id>` — snapshot + clone the source home, create from its
  recorded base tag. Whole-home only; there is no partial-dir copy.
- Every clone sees every home at `~/clones/<id>` — read or copy straight across, no
  server round-trip.
- `rmng clone exec <clone> -- <argv…>` — run one non-interactive command inside another clone
  (docker-exec style). Flags: `-u <user>`, `-w <dir>`, `-e KEY=VAL` (repeatable), `-d`/`--detach`
  (fire-and-forget: return immediately, no captured output). Passes through the command's exit
  code. As the agent user it inherits the clone's live desktop session env (`WAYLAND_DISPLAY`,
  `DISPLAY`, the session `PATH`, …), so **`-d` launches a GUI app on a headed clone's desktop**:
  `rmng clone exec -d pega-we-142 -- gnome-text-editor`. Example:
  `rmng clone exec pega-we-142 -- ls -la /home/rmng`.
- `rmng desktop <clone> <verb>` — drive another clone's desktop for computer use (**headed
  clones only** — see above; each action returns a fresh screenshot; add `--json` for
  `{screenshot, text}`). Verbs: `screenshot`,
  `monitors`, `windows`, `move X Y`, `click [X Y]`, `right-click`, `middle-click`,
  `double-click`, `scroll`, `key <chord>`, `type <text>`, `move-window <id>`.
  Example: `rmng desktop pega-we-142 screenshot`. To *open* an app, use `rmng clone exec -d`
  (above), not `desktop`. Read "Desktop coordinates" below before you send any click.

## Desktop coordinates

The screenshot you get back and the `x`/`y` you send share one coordinate space. **That space
is not the monitor's native resolution by default.** The daemon scales both to 1080p height,
so 1920x1080 on a 16:9 monitor. A screenshot captured at native resolution by some other tool
will not line up with these coordinates.

- No flag: the 1080p-height space. Read the returned screenshot, click in those same numbers.
- `--native`: use the monitor's real resolution for this call.
- `--resolution <W>x<H>`: use an explicit space, for example `--resolution 1280x720`.

Pass the same choice to every call in a sequence. Because one flag sets both the image and
the coordinates, the two can never disagree within a call. They do disagree if you screenshot
with `--native` and then click without it.

These two flags work on `screenshot`, `move`, `click`, `right-click`, `middle-click`,
`double-click`, and `scroll`. The other desktop flags:

- `--monitor <n>`: target one monitor. `rmng desktop <clone> monitors` lists the ids. Works on
  the seven verbs above plus `move-window`.
- `--out <path>`: also write the returned screenshot to a file. Works on the seven verbs above
  plus `key` and `type`.
- `--mode <mode>`: placement for `move-window <id>`, for example `maximize` or `center-half`.

## Create clones

Four create verbs. They share the flags in "Common create flags" below; clone images come from the preset Dockerfile (gen-2), so there is no image flag.

- `rmng clone create <hostname>` — exact hostname (a DNS label), no ticket.
  Takes `--preset <name>` / `--no-preset`.
- `rmng clone create-from-ticket <link-or-id>` — clone for an EXISTING Linear ticket. The
  hostname derives from the ticket id (`WE-142` → `<prefix>we-142`) and **the preset is
  auto-selected from the ticket's team prefix** — there is deliberately no `--preset` here.
  Also takes `--agent-instructions` / `--claude-instructions` (appended to the defaults,
  taking precedence).
- `rmng clone create-with-new-ticket --team <key> --title <t>` — CREATE a Linear ticket,
  then clone for it. `--team` is a Linear team key like `we`, and it must be a label on some
  preset: that preset is the one used, and its Linear API key opens the issue. Description via
  `--description <markdown>` or `--description-file <path>` (`-` = stdin, which is the sane
  way to pass a multi-line body). Same instruction flags as `create-from-ticket`.
- `rmng clone create-plain --title <t>` — no-ticket clone with a title-derived
  hostname. `--message`/`--message-file` is auto-sent to the agent as its first message;
  `--preset <name>` is required when any presets are configured.

### Common create flags

- `--wait` (with `--timeout <secs>`, default 600) — block until the clone is fully created,
  streaming progress. **Without it the command returns as soon as the operation starts**, so
  use `--wait` whenever the next step needs the clone to exist.
- `--claude-account <sel>` / `--codex-account <sel>` — the account for each provider,
  independently. A selection is an email (pin it), `auto` (the server picks), `none` (no
  token at all), or `group:<pool>` (bind to a named pool and let the rotator balance it).
  Omitting one walks parent → the preset's default → `auto`.
- `--headless` — no desktop (see "Headed vs headless" above). Default is headed.
- `--parent <clone>` — nest under a specific top-level clone. `--top-level` forces a
  top-level clone instead.

**Run from inside a clone, a new clone auto-nests as a sub clone under you AND inherits your
account selections and env preset by default** — a helper you spin up shares your accounts and
preset with no flags. What it inherits is the *selection*, not the account you happen to be
running: if you are on `auto`, so is it, and it gets its own pick. `--top-level` skips both.

## Retire clones

- `rmng clone rm <clone> [-y]` — destroy a clone (prompts unless `-y`; also removes its sub clones).
  Non-interactive callers MUST pass `-y`.
- `rmng clone archive <clone>` / `rmng clone restore <clone>` — stop-and-retain, then bring back.
- `rmng account swap <clone> <sel> [--codex]` — change a clone's account for one provider.
  Takes the same selection forms as the create flags. The token is written into the clone's
  credential file immediately; nothing restarts.

## Search what other clones have already done

The control-server keeps a greppable copy of every clone's Claude Code, Cursor and Codex
transcripts, and keeps it after the clone is gone. Subagent turns are in there too, which on a
session that delegates heavily is most of it. Search it before solving something from scratch:
the odds are good that another clone hit the same wall, and its reasoning is still there.

- `rmng ledger search <pattern> [--clone <id>] [--since <when>] [--until <when>] [--limit <N>]`
  — case-insensitive substring over whole ledger lines, so it matches the text, the tool name
  and the record kind alike. `--since`/`--until` take a duration ago (`90m`, `6h`, `2d`, `3w`)
  or epoch milliseconds. Newest first, capped at `--limit` (default 50, server maximum 500).
  Columns are `CLONE WHEN KIND SESSION OFFSET TEXT`, and the session and offset are the two
  arguments the next command takes, so a hit worth following up is already a command.
  Example: `rmng ledger search "va-api" --since 2d`.
- Three flags split a session in two. `--sidechain` keeps only subagent turns, `--no-sidechain`
  only the conversation somebody had, and `--agent <id>` reads back one subagent's whole run.
  Reach for `--no-sidechain` when a fan-out is burying the answer, and `--sidechain` when the
  thing you want is a reviewer's or researcher's report rather than the chat around it. The id
  `--agent` takes is the `agentId` on a hit, which `--json` shows:
  `rmng ledger search "Here is my review" --sidechain --json | jq -r '.hits[].line | fromjson.agentId'`.
- `rmng ledger read <clone> <session> [--offset <N>] [--len <N>]` — the conversation around a
  hit. Pass the hit's own offset to re-read that line, or less to read what led up to it. The
  range snaps outward to line boundaries, so stdout is always whole NDJSON lines. Default
  `--len 65536`, server maximum 1 MiB. The `bytes A..B of C` envelope goes to stderr, so
  piping needs no flag:
  `rmng ledger read pega-we-142 793f5eac-… --offset 4096 | jq -r '.kind + ": " + .text'`.

## Preset images & accounts

- Clone images build on demand from each preset's Dockerfile into a hash tag; unused tags are purged automatically on delete.
- `rmng account ls [--provider claude|codex]` — list imported accounts + usage windows.
- `rmng account rm <email> [--codex]` — delete an imported account, moving any clones off it.

## Tips

- Prefer `rmng clone exec <clone> -- …` over hand-rolled SSH when you just need to run one
  command elsewhere.
- Everything is addressed by **clone id** (the first column of `rmng clone ls`).
- `rmng clone select <clone>` points the operator's *viewer* at a clone — it does NOT change
  which clone your other commands target. `rmng clone select --none` clears the selection.
- `--wait` and `--timeout <secs>` are not create-only. Both also work on `rmng clone rm`,
  `rmng clone archive`, and `rmng clone restore`.
  Use `--wait` on a pull, which can run for many minutes.
"#;

/// The `rmng-cli` skill TarEntries: the same SKILL.md at both skill locations.
/// The activity probe, written to `~/.rmng/hook.py` and registered in Claude Code's
/// `settings.json`. It is how the server tells a clone that is thinking from one that is
/// waiting on a person: see [`crate::stuck`].
///
/// It runs on someone's live working clone, on every tool call, so it is built to be boring.
/// It never raises, never blocks, and never writes anywhere but its own log. Every failure
/// path still exits 0, because a hook that exits non-zero interrupts the agent.
const RMNG_HOOK_PY: &str = r#"#!/usr/bin/env python3
"""RMNG activity probe. One hook invocation appends one line, in one write().

Installed and kept current by the control-server's clone reconciler. Editing this file in a
clone accomplishes nothing: the reconciler stamps a hash of it and restores it within 30s.
"""
import json
import os
import sys
import time

LOG = os.path.expanduser("~/.rmng/agent-events.jsonl")
CAP = 64 * 1024 * 1024  # never grow without bound on a clone nobody is watching

KEEP = ("hook_event_name", "session_id", "agent_id", "agent_type", "cwd")


def main():
    try:
        event = json.load(sys.stdin)
    except Exception as exc:
        event = {"hook_event_name": "PARSE_ERROR", "error": str(exc)}
    if not isinstance(event, dict):
        event = {"hook_event_name": "NON_DICT"}

    record = {k: event.get(k) for k in KEEP}
    record["ts"] = time.time()

    # Stop carries the live outstanding-work set and the agent's own closing words. Both are
    # the whole answer to "did this turn really end", so they are worth their bytes.
    if isinstance(event.get("background_tasks"), list):
        record["background_tasks"] = event["background_tasks"][:20]
    if isinstance(event.get("last_assistant_message"), str):
        record["last_assistant_message"] = event["last_assistant_message"][:700]
    # `status` is how Cursor reports the way a turn ended: completed, aborted, or error.
    # Claude Code splits the same three across Stop and StopFailure.
    for extra in ("tool_name", "tool_use_id", "reason", "status"):
        if extra in event:
            record[extra] = str(event[extra])[:120]
    if "tool_input" in event:
        record["tool_input"] = json.dumps(event["tool_input"])[:400]

    line = (json.dumps(record, separators=(",", ":")) + "\n").encode()
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    try:
        if os.path.getsize(LOG) > CAP:
            return
    except OSError:
        pass
    fd = os.open(LOG, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(fd, line)
    finally:
        os.close(fd)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass  # a probe must never be the reason an agent stops
"#;

/// The path Claude Code runs, as the CLONE sees it.
///
/// Everything else here works in host coordinates (`/proc/<pid>/root/home/rmng/…`), and
/// writing one of those into `settings.json` produces a command that cannot exist inside the
/// container. It fails in the worst way available: silently to us, and as a red hook error on
/// every single tool call to whoever is working in that clone.
const HOOK_IN_CLONE: &str = "/home/rmng/.rmng/hook.py";

/// The events the probe subscribes to.
///
/// `SessionStart`, `Notification` and `ConfigChange` are deliberately absent: they carry
/// nothing the decision uses. `PreToolUse`/`PostToolUse` are the expensive pair, roughly 33 ms
/// each per tool call, and they stay because an unmatched `PreToolUse` is what keeps a clone
/// mid-command from reading as finished. Measured over the 323 fleet samples that carry one:
/// 8 false alarms with that evidence, 41 without.
const HOOK_EVENTS: [&str; 9] = [
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
];

/// The same probe, in Cursor's own vocabulary.
///
/// Cursor reads `~/.claude/settings.json` as well and converts it, so this looks redundant.
/// It is not. The converter drops three events it has no Claude name for, and one of them is
/// `postToolUseFailure` — which is the ONLY event Cursor fires when a tool call fails. Going
/// through the converter alone, every failed tool call stays unmatched forever and the clone
/// reads as blocked inside a command that finished. Registering natively also picks up
/// `subagentStart`, the other event the converter drops.
///
/// Cursor fires both registrations, so most events arrive twice.
/// [`crate::stuck::read_hook_events`] drops the duplicate.
const CURSOR_HOOK_EVENTS: [&str; 8] = [
    "beforeSubmitPrompt",
    "stop",
    "sessionEnd",
    "subagentStart",
    "subagentStop",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
];

pub(crate) fn rmng_hook_entries() -> Vec<TarEntry> {
    vec![TarEntry {
        path: "home/rmng/.rmng/hook.py".to_string(),
        data: RMNG_HOOK_PY.as_bytes().to_vec(),
        mode: 0o755,
        uid: CLONE_UID,
        gid: CLONE_GID,
    }]
}

/// Register the probe in `~/.claude/settings.json` without disturbing the operator's own keys.
///
/// That file is Claude Code's own user state: `model`, `theme`, `effortLevel`,
/// `enabledPlugins`, whatever it adds next. So this sets `.hooks` and touches nothing else,
/// the same merge shape [`merge_claude_mcp`] uses on `~/.claude.json` for the same reason.
/// Assigning the whole `.hooks` object rather than merging into it is deliberate: it is how a
/// renamed or dropped event stops firing, instead of lingering forever the way a
/// merge-only-what-we-emit would leave it.
/// The `.hooks` object both agents get, built once and shared by the merge script and the
/// pre-boot initial files below. Single source for the event lists AND the entry shape.
fn claude_hooks_object() -> serde_json::Value {
    serde_json::Value::Object(
        HOOK_EVENTS
            .iter()
            .map(|event| {
                (
                    (*event).to_string(),
                    serde_json::json!([{
                        // Claude Code reads a missing matcher as "every tool". Cursor reads this
                        // same file and its converter calls `.split()` on the matcher without
                        // checking for one, so a missing matcher throws and Cursor silently
                        // registers NONE of these hooks. Spelling out the default costs nothing
                        // and is the whole difference between the probe working in Cursor and
                        // not existing there.
                        "matcher": "*",
                        "hooks": [{ "type": "command", "command": HOOK_IN_CLONE, "timeout": 10 }]
                    }]),
                )
            })
            .collect(),
    )
}

fn cursor_hooks_object() -> serde_json::Value {
    serde_json::Value::Object(
        CURSOR_HOOK_EVENTS
            .iter()
            .map(|event| {
                (
                    (*event).to_string(),
                    serde_json::json!([{ "command": HOOK_IN_CLONE, "timeout": 10 }]),
                )
            })
            .collect(),
    )
}

/// Initial `~/.claude/settings.json`: what the hook merge assigns on an empty base
/// (`.hooks = {...}`, whole-object assignment — renames/drops stop firing instead of
/// lingering). Other keys (model, theme) only exist on lived-in clones, where the loop
/// merge — not this — applies.
pub(crate) fn claude_settings_initial() -> String {
    serde_json::json!({ "hooks": claude_hooks_object() }).to_string()
}

/// Initial `~/.cursor/hooks.json`: `.version = 1 | .hooks = {...}` on an empty base.
pub(crate) fn cursor_hooks_initial() -> String {
    serde_json::json!({ "version": 1, "hooks": cursor_hooks_object() }).to_string()
}

/// Merge the probe registration into a `~/.claude/settings.json` body: whole-`.hooks`
/// assignment, everything else untouched. Pure-Rust port of the old jq merge — assigning
/// (not deep-merging) is deliberate, so a renamed or dropped event stops firing instead
/// of lingering. A non-object base is a hard error, matching the old merge.
pub(crate) fn merge_claude_hooks(
    base: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    set_json_key(base, "~/.claude/settings.json", "hooks", claude_hooks_object())
}

/// Merge the probe registration into a `~/.cursor/hooks.json` body (`.version = 1` plus
/// whole-`.hooks` assignment). Same port, same rules.
pub(crate) fn merge_cursor_hooks(
    base: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let with_version = set_json_key(base, "~/.cursor/hooks.json", "version", serde_json::json!(1))?;
    set_json_key(
        &with_version,
        "~/.cursor/hooks.json",
        "hooks",
        cursor_hooks_object(),
    )
}

fn claude_hook_stamp_path() -> &'static str {
    "etc/rmng/claude-hook"
}

/// Keyed on a hash of the probe plus its registration, so editing [`RMNG_HOOK_PY`] or
/// [`HOOK_EVENTS`] re-pushes to every clone with no manual version bump to remember.
fn claude_hook_desired() -> String {
    let mut entries = rmng_hook_entries();
    for (path, data) in [
        ("registration-claude", claude_settings_initial()),
        ("registration-cursor", cursor_hooks_initial()),
    ] {
        entries.push(TarEntry {
            path: path.into(),
            data: data.into_bytes(),
            mode: 0,
            uid: 0,
            gid: 0,
        });
    }
    desired_payload_hash(&entries)
}

pub(crate) fn claude_hook_stamp_entry() -> TarEntry {
    TarEntry {
        path: claude_hook_stamp_path().to_string(),
        data: format!("{}\n", claude_hook_desired()).into_bytes(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

fn rmng_cli_skill_entries() -> Vec<TarEntry> {
    [
        "home/rmng/.claude/skills/rmng-cli/SKILL.md",
        "home/rmng/.agents/skills/rmng-cli/SKILL.md",
    ]
    .into_iter()
    .map(|path| TarEntry {
        path: path.to_string(),
        data: RMNG_CLI_SKILL_MD.as_bytes().to_vec(),
        mode: 0o644,
        uid: CLONE_UID,
        gid: CLONE_GID,
    })
    .collect()
}

/// The per-clone agent config bundle: the shared **global agent prompt** (layers a+c, passed in
/// as `global_prompt`) written to every agent's native rules file — Claude Code's
/// `~/.claude/CLAUDE.md`, Codex's `~/.codex/AGENTS.md`, and pi's `~/.pi/agent/AGENTS.md` —
/// plus the generated Codex config and
/// the neutral MCP descriptor the node-agent reads. Identical body in both rules files, so a
/// single source drives every agent's operating memory. The content-hash stamp on this set means
/// a Settings edit to layer a/c re-applies on the next pass.
pub(crate) fn codex_parity_entries(headless: bool, global_prompt: &str) -> Vec<TarEntry> {
    let guidance = |path: &str| TarEntry {
        path: path.to_string(),
        data: global_prompt.as_bytes().to_vec(),
        mode: 0o644,
        uid: CLONE_UID,
        gid: CLONE_GID,
    };
    let entries = vec![
        // The global agent prompt (a+c), one identical body per agent's rules location.
        guidance("home/rmng/.claude/CLAUDE.md"),
        guidance("home/rmng/.codex/AGENTS.md"),
        // pi (the node-agent's embedded coding agent) reads its own global context file. It
        // recognises AGENTS.md and CLAUDE.md at the working directory and above, but the only
        // location it always loads is this one, and the wrapper runs with cwd = the clone home.
        guidance("home/rmng/.pi/agent/AGENTS.md"),
        // Cursor reads neither of those. Its own user-level rules are `.mdc` files under
        // `~/.cursor/rules`, which is where it looks: `joinPath(userHome, ".cursor", "rules")`
        // in its bundle, and nothing at the home level named AGENTS.md or CLAUDE.md is read at
        // all (it recognises those two only at a workspace root). Without this a clone worked
        // through Cursor ran with none of the operating memory every other agent gets.
        //
        // `alwaysApply: true` is what makes it unconditional rather than a rule the agent has
        // to choose. Verified on a live clone: the file was written, and a fresh Cursor agent
        // asked to quote a marker line out of its always-applied rules quoted it back.
        TarEntry {
            path: "home/rmng/.cursor/rules/rmng.mdc".to_string(),
            data: cursor_rule(global_prompt).into_bytes(),
            mode: 0o644,
            uid: CLONE_UID,
            gid: CLONE_GID,
        },
        // The neutral MCP descriptor the node-agent (agent-wrapper) reads (single source of
        // truth: `managed_mcp`). Headless-filtered here so the wrapper needs no headless logic.
        TarEntry {
            path: "home/rmng/.config/rmng/mcp.json".to_string(),
            data: mcp_descriptor_json(headless).into_bytes(),
            mode: 0o644,
            uid: CLONE_UID,
            gid: CLONE_GID,
        },
    ];
    // The `rmng-cli` skill, at both skill locations (Claude/Cursor + Codex).
    let mut entries = entries;
    entries.extend(rmng_cli_skill_entries());
    entries
}

/// The global prompt as one always-applied Cursor rule.
///
/// Cursor's `.mdc` is YAML front matter over a markdown body. `description` is what its Rules
/// list shows, and `alwaysApply` is the only field that decides whether the body is in the
/// agent's context at all.
fn cursor_rule(global_prompt: &str) -> String {
    format!(
        "---\ndescription: RMNG fleet operating memory\nalwaysApply: true\n---\n\n{global_prompt}"
    )
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
    // Single value source with the loop's own write path (`ensure_codex_parity` stamps
    // `codex_parity_desired`): hashing entries alone wrote a stamp the loop never matches,
    // so every fresh clone ate one redundant re-push.
    codex_parity_stamp_entry(&codex_parity_desired(entries))
}

/// Read a JSON guest file for a merge: missing or blank seeds the merge base as `{}`.
/// Anything present-but-unparseable is a hard error — matching the old jq merges, which
/// failed on those rather than silently repairing a file the operator may be editing.
/// Reads straight from the clone's live home (merged view), no daemon roundtrip.
async fn read_json_merge_base(
    _app: &App,
    clone_id: &str,
    rel_path: &str,
    label: &str,
) -> Result<serde_json::Value> {
    let raw = match crate::home_overlay::read_clone_home(clone_id, rel_path)
        .with_context(|| format!("{clone_id}: reading {label}"))?
    {
        None => return Ok(serde_json::json!({})),
        Some(bytes) => bytes,
    };
    let text = String::from_utf8_lossy(&raw);
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&text)
        .with_context(|| format!("{clone_id}: {label} is not valid JSON"))
}

/// Write one managed guest file straight into the clone's live home (atomic temp +
/// rename, 0600, clone-owned) — the clone sees it instantly, with no guest shell and no
/// tar roundtrip.
async fn upload_guest_file(
    _app: &App,
    clone_id: &str,
    rel_path: &str,
    data: Vec<u8>,
    label: &str,
) -> Result<()> {
    upload_guest_file_at_mode(_app, clone_id, rel_path, data, 0o600, label).await
}

/// [`upload_guest_file`] with an explicit mode (hook registrations are 0644, not 0600).
async fn upload_guest_file_at_mode(
    _app: &App,
    clone_id: &str,
    rel_path: &str,
    data: Vec<u8>,
    mode: u32,
    label: &str,
) -> Result<()> {
    crate::home_overlay::write_clone_home(clone_id, rel_path, &data, mode)
        .with_context(|| format!("{clone_id}: writing {label}"))
}

/// Interactive Claude Code (and the inner Cursor agent / any human `claude`) reads its MCP servers
/// from `~/.claude.json`'s top-level `mcpServers` key. That file is state-bearing (Claude Code
/// accumulates project history in it), so we **merge** the two managed servers rather than
/// overwrite it — matching how the template seeds `linear` (`template/setup/30-user.sh`). `linear`
/// is always set; `desktop` (the clone-daemon computer-use MCP on :9004) is set on headed clones
/// and deleted on headless ones (there is no daemon there). `${LINEAR_API_KEY}` is stored literally
/// — Claude Code expands it from the session env at runtime.
fn claude_mcp_stamp_path() -> &'static str {
    "etc/rmng/claude-mcp"
}

/// Desired stamp value — a hash of the merge script itself, so the headless bit, a
/// managed-set code change, and any future change to the merge all re-apply on the next
/// convergence trigger. (A `v1 headless=…` tag used to live here; it never re-pushed on
/// code changes.)
/// Desired stamp value — a hash of the canonical merge output, so the headless bit, a
/// managed-set code change, and any future change to the merge all re-apply on the next
/// convergence trigger. (A `v1 headless=…` tag used to live here; it never re-pushed on
/// code changes.)
fn claude_mcp_desired(headless: bool) -> String {
    // `expect`: merging onto `{}` cannot fail (only a non-object base errors).
    let canonical = merge_claude_mcp(&serde_json::json!({}), headless)
        .expect("empty object takes the Claude MCP merge")
        .to_string();
    desired_payload_hash(&[TarEntry {
        path: "claude-mcp".into(),
        data: canonical.into_bytes(),
        mode: 0,
        uid: 0,
        gid: 0,
    }])
}

pub(crate) fn claude_mcp_stamp_entry_for(headless: bool) -> TarEntry {
    TarEntry {
        path: claude_mcp_stamp_path().to_string(),
        data: format!("{}\n", claude_mcp_desired(headless)).into_bytes(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

/// Stamp value for the Codex-parity step: the payload bytes. The parent directories the
/// entries land in are pre-created by the template with the right owner (see phase 30),
/// so no prepare script rides the stamp anymore.
pub(crate) fn codex_parity_desired(entries: &[TarEntry]) -> String {
    desired_payload_hash(entries)
}

/// Codex CLI install (post-boot repair) is gone: the template bakes `codex` and is
/// its sole source. An install-if-missing script in three places was one truth in
/// three copies; the image won.

/// Prepare a clone's filesystem for the `authorized_keys` upload: just the two directories the
/// tar entries land in. Creating `~/.ssh` 700 root-owned-by-rmng matters because sshd's
/// `StrictModes` refuses a group/world-writable one.
///
/// This deliberately touches NOTHING else under `~/.ssh`. It used to also delete
/// `~/.ssh/id_ed25519` when that file matched the shared fleet key — part of relocating that key
/// out of `~/.ssh` to stop GNOME's `gcr-ssh-agent` crashing on it. Both the fleet key and that
/// migration are gone: the server no longer provisions any client identity, so there is no
/// rmng-owned private key in `~/.ssh` to clean up, and a user's own keys there are none of our
/// business. (A clone still carrying the old `~/.ssh/id_ed25519` keeps it — it is the user's file
/// now, and the fleet pubkey is no longer in any `authorized_keys`, so it authenticates nothing.)
pub(crate) fn ssh_prepare_script() -> String {
    String::from(
        "set -e\n\
         install -d -o rmng -g rmng -m700 /home/rmng/.ssh\n\
         mkdir -p /etc/ssh\n",
    )
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

/// Restart the clone-daemon after a binary refresh — but only if its unit is present AND
/// unmasked. Headless clones MASK `rmng-clone-daemon.service` (symlink → /dev/null, laid
/// pre-boot by the create path), so a bare `systemctl --user restart` would fail on the mask
/// and — under `set -e` — abort the whole payload reconcile before the agent-wrapper
/// restart + payload stamp ever run, permanently wedging binary refreshes. Guard on
/// `systemctl cat` (absent ⇒ skip) plus a `readlink` mask check (masked ⇒ skip); a real
/// restart failure still surfaces under `set -e` on headed clones.
///
/// The session holder is neither started nor restarted here, only enabled so it comes back on the
/// next boot. It holds the clone's Mutter session and virtual monitors, and restarting it is
/// exactly what resets every window position: gnome-shell remaps them the moment the monitor set
/// empties. Starting it is left to the daemon, which does it after connecting to the media socket
/// and finding no holder. That ordering matters on the release that introduces the holder, where
/// starting it here would raise a second set of monitors alongside the outgoing daemon's and give
/// `apply_layout` two identical connectors to choose between.
///
/// The seed above the restart is for the clone that has never run a holder. The shipped holder
/// unit carries no layout on purpose (one unit ships to every clone, and a baked layout would be
/// wrong the moment the operator changes presets), so a holder with nothing remembered comes up
/// on the built-in single 1920x1080. On a fleet upgrading onto the holder that is every clone at
/// once, each one landing on a desktop the operator never chose. Writing the active preset into
/// the holder's own memory first makes its first session the right one. Only when the file is
/// absent: after that the holder owns it, and overwriting would drag a clone back off the layout
/// it was last viewed with.
fn restart_clone_daemon_script(monitors: &str) -> String {
    format!(
        r#"set -e
run_user() {{ runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 "$@"; }}
unit=/home/rmng/.config/systemd/user/rmng-clone-daemon.service
if run_user systemctl --user cat rmng-clone-daemon.service >/dev/null 2>&1 && {{ [ ! -L "$unit" ] || [ "$(readlink "$unit")" != "/dev/null" ]; }}; then
  if [ ! -e /home/rmng/.rmng/monitors ]; then
    install -d -o rmng -g rmng /home/rmng/.rmng
    printf '%s\n' '{monitors}' > /home/rmng/.rmng/monitors
    chown rmng:rmng /home/rmng/.rmng/monitors
    echo "seeded the session holder's layout: {monitors}"
  fi
  run_user systemctl --user daemon-reload
  run_user systemctl --user enable rmng-session-holder.service >/dev/null 2>&1 || true
  run_user systemctl --user restart rmng-clone-daemon.service
else
  echo "rmng-clone-daemon.service absent or masked (headless clone) — skipping restart"
fi
"#
    )
}

/// One monitor layout in the `WxH+X+Y[*]` form the session holder reads, the same form
/// `RMNG_MONITORS` uses. A trailing `*` marks the primary.
fn monitors_csv(monitors: &[wire::MonitorSpec]) -> String {
    monitors
        .iter()
        .map(|m| {
            format!(
                "{}x{}+{}+{}{}",
                m.width,
                m.height,
                m.x,
                m.y,
                if m.primary { "*" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn restart_agent_wrapper_script() -> &'static str {
    r#"set -e
runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user restart agent-wrapper.service
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

fn etc_environment_sync_script(desired_env: &str) -> String {
    let desired_b64 = B64.encode(desired_env);
    // Gen-2 images carry no stale env, so the strip-list is exactly the desired keys.
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
grep -E '^[A-Za-z_][A-Za-z0-9_]*=' "$desired" | sed 's/=.*//' | sed '/^$/d' | sort -u > "$keys_file"
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
# The caller keys the agent-wrapper restart off this exact line: /etc/environment is read by
# PAM at session start, so a process already running keeps the environment it was launched
# with FOREVER. Writing the file is therefore only half the job — see `ENV_CHANGED_MARKER`.
echo "{marker}"
"#,
        marker = ENV_CHANGED_MARKER,
    )
}

/// Printed by [`etc_environment_sync_script`] only when it actually rewrote `/etc/environment`
/// (it exits 0 silently when the content already matched). The reconciler keys the
/// agent-wrapper restart off this, so the restart happens on a real change and not on every
/// 30 s pass — restarting unconditionally would interrupt an in-flight chat turn twice a
/// minute, forever.
const ENV_CHANGED_MARKER: &str = "rmng: /etc/environment updated";

fn preset_for_clone<'a>(
    cfg: &'a wire::AppConfig,
    host: &wire::RmngClone,
) -> Option<&'a wire::Preset> {
    if let Some(name) = host.preset_name.as_deref().filter(|s| !s.trim().is_empty()) {
        if let Some(preset) = cfg.presets.iter().find(|p| p.name == name) {
            return Some(preset);
        }
    }
    if let Some(prefix) = host
        .linear_workspace
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        if let Some(preset) = crate::naming::pick_preset_by_prefix(&cfg.presets, prefix) {
            return Some(preset);
        }
        if let Some(preset) = cfg
            .presets
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(prefix))
        {
            return Some(preset);
        }
    }
    if let Some(label) = host
        .linear_label
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        if let Some(preset) = cfg.presets.iter().find(|p| {
            p.labels
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(label))
        }) {
            return Some(preset);
        }
    }
    None
}

/// Like [`exec_ok`], but reports whether the script printed `marker` on stdout. Used for the
/// `/etc/environment` sync, which is the only reconcile step whose *follow-up* (restarting
/// the agent-wrapper so it picks the new env up) must be conditional on it having changed
/// something.
async fn exec_ok_marked(
    app: &App,
    clone_id: &str,
    script: &str,
    label: &str,
    marker: &str,
) -> Result<bool> {
    let mut seen = false;
    let code = app
        .docker
        .exec_script(clone_id, script, &[], &[], |stream, line| {
            if stream == crate::docker::STREAM_OUT && line.contains(marker) {
                seen = true;
            }
            tracing::debug!(target: "clone_reconcile", "{clone_id} {label} {stream}: {line}");
        })
        .await
        .with_context(|| format!("{clone_id}: {label}"))?;
    if code != 0 {
        bail!("{clone_id}: {label} exited {code}");
    }
    Ok(seen)
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
            if stream == crate::docker::STREAM_OUT {
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
        == Some(SSH_STAMP_VERSION)
    {
        return Ok(());
    }
    exec_ok(app, clone_id, &ssh_prepare_script(), "prepare ssh dirs").await?;
    let entries = crate::ssh::clone_ssh_tar_entries(
        &app.config().data_dir,
        clone_id,
        &app.config().ssh.authorized_keys,
    )?;
    // `authorized_keys` goes straight into the live home (the prepare step above already
    // ensured `~/.ssh` 700); the host keys still ride the tar — they live under `/etc`.
    crate::home_overlay::write_clone_home(
        clone_id,
        ".ssh/authorized_keys",
        crate::ssh::render_authorized_keys(&app.config().ssh.authorized_keys).as_bytes(),
        0o600,
    )
    .with_context(|| format!("{clone_id}: writing ssh authorized_keys"))?;
    let etc_entries: Vec<_> = entries
        .into_iter()
        .filter(|e| !e.path.starts_with("home/"))
        .collect();
    if !etc_entries.is_empty() {
        app.docker
            .upload_tar(clone_id, etc_entries)
            .await
            .with_context(|| format!("{clone_id}: uploading ssh host keys"))?;
    }
    exec_ok(app, clone_id, ssh_bootstrap_script(), "bootstrap sshd").await?;
    app.docker
        .upload_tar(clone_id, vec![ssh_stamp_entry()])
        .await
        .with_context(|| format!("{clone_id}: writing ssh stamp"))?;
    Ok(())
}

async fn ensure_codex_parity(
    app: &App,
    clone_id: &str,
    headless: bool,
    global_prompt: &str,
) -> Result<bool> {
    let entries = codex_parity_entries(headless, global_prompt);
    // The prepare script rides the stamp because it owns the parent directories these entries
    // land in. Without that, adding a directory to it would never reach a clone already stamped
    // for this content, and the tar extract would create the dir root-owned instead.
    let desired = codex_parity_desired(&entries);
    if read_stamp(app, clone_id, codex_parity_stamp_path(), "codex parity")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }

    // Parent dirs come pre-created from the template (phase 30) with the right owner;
    // write_clone_home backstops the rest (creating + chowning as needed). Every entry
    // in this set lives under the home bind — anything else is a bug, fail loud.
    for e in &entries {
        let rel = e.path.strip_prefix("home/rmng/").with_context(|| {
            format!("{clone_id}: parity entry outside the home bind: {}", e.path)
        })?;
        crate::home_overlay::write_clone_home(clone_id, rel, &e.data, e.mode)
            .with_context(|| format!("{clone_id}: writing parity file {}", e.path))?;
    }
    app.docker
        .upload_tar(clone_id, vec![codex_parity_stamp_entry(&desired)])
        .await
        .with_context(|| format!("{clone_id}: writing Codex parity stamp"))?;
    Ok(true)
}

/// Keep interactive Claude Code's `~/.claude.json` MCP set in sync (desktop headed-only, linear
/// always). Read-merge-write against the clone's live home: no guest shell, and the operator's
/// project history in that file is never at the mercy of a heredoc. Stamped on the
/// canonical merge output so it only runs when the desired set changes — retrofitting
/// `desktop` onto existing headed clones and removing it from existing headless ones on
/// the reconciler's next pass.
async fn ensure_claude_mcp(app: &App, clone_id: &str, headless: bool) -> Result<bool> {
    let desired = claude_mcp_desired(headless);
    if read_stamp(app, clone_id, claude_mcp_stamp_path(), "claude mcp")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }
    let base = read_json_merge_base(app, clone_id, ".claude.json", "~/.claude.json")
        .await?;
    let merged = merge_claude_mcp(&base, headless)
        .with_context(|| format!("{clone_id}: merging ~/.claude.json MCP"))?;
    upload_guest_file(
        app,
        clone_id,
        ".claude.json",
        merged.to_string().into_bytes(),
        "~/.claude.json MCP",
    )
    .await?;
    app.docker
        .upload_tar(clone_id, vec![claude_mcp_stamp_entry_for(headless)])
        .await
        .with_context(|| format!("{clone_id}: writing claude mcp stamp"))?;
    Ok(true)
}

/// Keep Cursor's `~/.cursor/mcp.json` pointed at the same managed servers, so the agent a person
/// drives in the clone's IDE has the tools the CLI agents already have. Read-merge-write
/// against the clone's live home; stamped on a hash of the canonical output, so a headless flip or a
/// rotated Linear key re-applies on the next pass.
async fn ensure_cursor_mcp(
    app: &App,
    clone_id: &str,
    headless: bool,
    linear_key: &str,
) -> Result<bool> {
    let desired = cursor_mcp_desired(headless, linear_key);
    if read_stamp(app, clone_id, cursor_mcp_stamp_path(), "cursor mcp")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }
    let base =
        read_json_merge_base(app, clone_id, ".cursor/mcp.json", "~/.cursor/mcp.json")
            .await?;
    let merged = merge_cursor_mcp(&base, headless, linear_key)
        .with_context(|| format!("{clone_id}: merging ~/.cursor/mcp.json MCP"))?;
    upload_guest_file(
        app,
        clone_id,
        ".cursor/mcp.json",
        merged.to_string().into_bytes(),
        "~/.cursor/mcp.json MCP",
    )
    .await?;
    app.docker
        .upload_tar(
            clone_id,
            vec![cursor_mcp_stamp_entry_for(headless, linear_key)],
        )
        .await
        .with_context(|| format!("{clone_id}: writing cursor mcp stamp"))?;
    Ok(true)
}

/// Install the activity probe and register it in Claude Code's settings.
///
/// Claude Code reloads `settings.json` live, so an already-running agent picks the hooks up
/// with no restart. Confirmed on a 32-clone fleet: seven clones that were sitting idle at
/// install time logged events on their next turn without being touched.
async fn ensure_claude_hook(app: &App, clone_id: &str) -> Result<bool> {
    let desired = claude_hook_desired();
    if read_stamp(app, clone_id, claude_hook_stamp_path(), "claude hook")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }
    // Probe straight into the live home (0755: it executes); parents come pre-created
    // from the template, with write_clone_home as backstop.
    crate::home_overlay::write_clone_home(clone_id, ".rmng/hook.py", RMNG_HOOK_PY.as_bytes(), 0o755)
        .with_context(|| format!("{clone_id}: writing the activity probe"))?;
    // Registrations merge into the operator's own settings files (0644, as before).
    for (rel, label, merged) in [
        (
            ".claude/settings.json",
            "~/.claude/settings.json",
            merge_claude_hooks(
                &read_json_merge_base(app, clone_id, ".claude/settings.json", "~/.claude/settings.json").await?,
            ),
        ),
        (
            ".cursor/hooks.json",
            "~/.cursor/hooks.json",
            merge_cursor_hooks(
                &read_json_merge_base(app, clone_id, ".cursor/hooks.json", "~/.cursor/hooks.json").await?,
            ),
        ),
    ] {
        let merged = merged.with_context(|| format!("{clone_id}: merging {label} hooks"))?;
        upload_guest_file_at_mode(app, clone_id, rel, merged.to_string().into_bytes(), 0o644, label)
            .await?;
    }
    app.docker
        .upload_tar(clone_id, vec![claude_hook_stamp_entry()])
        .await
        .with_context(|| format!("{clone_id}: writing claude hook stamp"))?;
    Ok(true)
}

fn codex_mcp_stamp_path() -> &'static str {
    "etc/rmng/codex-mcp"
}

/// Desired stamp value — a hash of the canonical rendered tables (no secrets: the only
/// auth form here is the `LINEAR_API_KEY` env *name*), so the headless bit and any future
/// change to the managed set re-apply on the next pass. (The old `v1` tag never re-pushed
/// on managed-set code changes.)
fn codex_mcp_desired(headless: bool) -> String {
    desired_payload_hash(&[TarEntry {
        path: "codex-mcp".into(),
        data: codex_mcp_toml(headless).into_bytes(),
        mode: 0,
        uid: 0,
        gid: 0,
    }])
}

pub(crate) fn codex_mcp_stamp_entry_for(headless: bool) -> TarEntry {
    TarEntry {
        path: codex_mcp_stamp_path().to_string(),
        data: format!("{}\n", codex_mcp_desired(headless)).into_bytes(),
        mode: 0o644,
        uid: 0,
        gid: 0,
    }
}

/// Keep Codex's `~/.codex/config.toml` MCP tables in sync (desktop headed-only, linear always),
/// merging rather than overwriting so the operator's own settings in that file survive.
/// Read-merge-write against the clone's live home.
async fn ensure_codex_mcp(app: &App, clone_id: &str, headless: bool) -> Result<bool> {
    let desired = codex_mcp_desired(headless);
    if read_stamp(app, clone_id, codex_mcp_stamp_path(), "codex mcp")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }
    // Missing file merges from empty (the old script created it); TOML is line-merged so
    // nothing present-but-unusual can fail the parse — it passes through untouched.
    let current = crate::home_overlay::read_clone_home(clone_id, ".codex/config.toml")
        .with_context(|| format!("{clone_id}: reading ~/.codex/config.toml"))?;
    let text = current
        .as_deref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let merged = merge_codex_config(&text, headless);
    upload_guest_file(
        app,
        clone_id,
        ".codex/config.toml",
        merged.into_bytes(),
        "~/.codex/config.toml MCP",
    )
    .await?;
    app.docker
        .upload_tar(clone_id, vec![codex_mcp_stamp_entry_for(headless)])
        .await
        .with_context(|| format!("{clone_id}: writing codex mcp stamp"))?;
    Ok(true)
}

async fn ensure_payload_current(app: &App, clone_id: &str, headless: bool) -> Result<bool> {
    let entries = binary_payload_entries(headless)?;
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
        &restart_clone_daemon_script(&monitors_csv(&app.config().effective_monitors())),
        "restart rmng-clone-daemon",
    )
    .await?;
    exec_ok(
        app,
        clone_id,
        restart_agent_wrapper_script(),
        "restart agent-wrapper",
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

/// SSH-only sync for one clone. Returns false when the rest of the chain must be
/// skipped this time (SSH is the gate: without it no later exec can run).
async fn sync_clone_ssh(app: &App, id: &str, warned: &mut HashSet<String>) -> bool {
    match ensure_ssh_ready(app, id).await {
        Ok(()) => {}
        Err(e) => {
            if warned.insert(format!("{id}:ssh")) {
                tracing::warn!(target: "clone_reconcile", "clone {id}: ssh reconcile failed: {e:#}");
            } else {
                tracing::debug!(target: "clone_reconcile", "clone {id}: ssh reconcile still failing: {e:#}");
            }
            return false;
        }
    }
    warned.remove(&format!("{id}:ssh"));
    true
}

/// Full content convergence for one clone: SSH, env, parity, the three MCP merges, the
/// activity probe, and the payload refresh. Idempotent and stamped throughout: re-running
/// a converged clone is a handful of `cat`s plus one env compare.
///
/// Callers (nothing here runs on a timer): the boot pass, Settings-save fan-out, and
/// post-op convergence after fork/rebase/migrate. The 30 s loop runs SSH only.
async fn sync_clone_contents(app: &App, h: &wire::RmngClone, warned: &mut HashSet<String>) {
    let id = h.id.as_str();
    if !app.docker.is_running(id).await.unwrap_or(false) {
        return;
    }
    if !sync_clone_ssh(app, id, warned).await {
        return;
    }
    let cfg = app.config();
    // An unresolvable control host breaks this clone's env identically: skip it
    // (warn-once) rather than rewriting it into a degraded URL. It cannot heal
    // call-over-call — the error below names the broken network config to fix.
    let control_env = match crate::provision::control_env_vars(app).await {
        Ok(env) => {
            warned.remove(format!("{id}:control-env").as_str());
            env
        }
        Err(e) => {
            if warned.insert(format!("{id}:control-env")) {
                tracing::warn!(target: "clone_reconcile", "clone {id}: control host unresolvable, skipping: {e:#}");
            }
            return;
        }
    };

        let mut desired_env = control_env.clone();
        // Per-clone identity key (`RMNG_PROXY_KEY`): recomputed into `/etc/environment` on every
        // resync so an existing clone picks it up without a recreate. Minted + persisted
        // server-side; never serialized onto `RmngClone`/state. See `crate::clonekey`.
        desired_env.extend(crate::provision::clone_key_env_vars(app, id));
        if let Some(preset) = preset_for_clone(&cfg, h) {
            desired_env.extend(crate::provision::preset_env_vars(preset));
        } else if h.preset_name.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            // Warn-once: stripping the keys would wipe live config on a preset rename, so
            // preservation is the behavior — the warn only needs saying once per clone.
            if warned.insert(format!("{}:preset", id)) {
                tracing::warn!(
                    target: "clone_reconcile",
                    "clone {id}: preset {:?} no longer exists; preserving unmanaged /etc/environment keys",
                    h.preset_name
                );
            }
        }
        // Claude Code's default model (ANTHROPIC_MODEL). The create path seeds this same var
        // from the same helper, so a fresh clone already has it and this pass is a no-op
        // content-compare rather than a 30 s-late rewrite.
        desired_env.push(claude_model_env_var());
        // Read before the render below shadows the list: Cursor cannot expand an environment
        // reference in its MCP config, so its `linear` server needs the value itself.
        let linear_key = env_value(&desired_env, "LINEAR_API_KEY");
        let desired_env = crate::provision::clone_etc_environment_conf(&desired_env);
        let env_script = etc_environment_sync_script(&desired_env);
        match exec_ok_marked(
            app,
            id,
            &env_script,
            "sync /etc/environment",
            ENV_CHANGED_MARKER,
        )
        .await
        {
            Ok(changed) => {
                warned.remove(&format!("{id}:etc-env"));
                // Writing /etc/environment does NOT reach the processes already running: PAM
                // reads it at session start, so the long-lived agent-wrapper keeps whatever it
                // was launched with. It fronts the chat panel, so on a clone that predates an
                // env change (the group-proxy split moved ANTHROPIC_BASE_URL) chat would talk
                // to the old endpoint until something restarted it by hand. Restart it here —
                // only on a real change, so an in-flight turn isn't interrupted every pass.
                if changed {
                    tracing::info!(target: "clone_reconcile", "clone {id}: /etc/environment changed — restarting agent-wrapper to pick it up");
                    if let Err(e) = exec_ok(
                        app,
                        id,
                        restart_agent_wrapper_script(),
                        "restart agent-wrapper (env change)",
                    )
                    .await
                    {
                        tracing::warn!(target: "clone_reconcile", "clone {id}: agent-wrapper restart after env change failed: {e:#}");
                    }
                }
            }
            Err(e) => {
                if warned.insert(format!("{id}:etc-env")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: /etc/environment reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: /etc/environment reconcile still failing: {e:#}");
                }
            }
        }

        // Codex CLI is the template's (baked, sole source) — no post-boot install step.

        // `gpt_models` (this clone's group GPT list, or the FALLBACK_GPT_MODELS safety net) was
        // resolved once per pass above, alongside the Claude Code default, from the group catalog.
        // The global agent prompt (layers a+c) is composed from config + this clone's preset, so a
        // Settings edit re-applies to existing clones on the next pass (content-hash-stamped).
        let global_prompt = crate::web::compose_global_prompt(&cfg, preset_for_clone(&cfg, h));
        match ensure_codex_parity(app, id, h.headless, &global_prompt).await {
            Ok(true) => {
                warned.remove(&format!("{id}:codex"));
                tracing::info!(
                    target: "clone_reconcile",
                    "clone {id}: refreshed agent prompt (CLAUDE.md/AGENTS.md) and MCP config"
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
                // No gate: later steps own independent stamps and dirs (template-made),
                // so a parity failure must not hold MCP/hook convergence hostage.
            }
        }

        // Interactive Claude Code's `~/.claude.json` MCP set (desktop headed-only + linear). jq
        // merge, stamped on the headless bit. Best-effort — a failure is logged and retried.
        match ensure_claude_mcp(app, id, h.headless).await {
            Ok(true) => {
                warned.remove(&format!("{id}:claude-mcp"));
                tracing::info!(
                    target: "clone_reconcile",
                    "clone {id}: synced ~/.claude.json MCP servers (headless={})",
                    h.headless
                );
            }
            Ok(false) => {
                warned.remove(&format!("{id}:claude-mcp"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:claude-mcp")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: ~/.claude.json MCP reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: ~/.claude.json MCP reconcile still failing: {e:#}");
                }
            }
        }

        // Cursor's `~/.cursor/mcp.json`, the same managed set the CLI agents get. Merged, not
        // rewritten: the operator's own servers share that file.
        match ensure_cursor_mcp(app, id, h.headless, &linear_key).await {
            Ok(true) => {
                warned.remove(&format!("{id}:cursor-mcp"));
                tracing::info!(
                    target: "clone_reconcile",
                    "clone {id}: synced ~/.cursor/mcp.json MCP servers (headless={})",
                    h.headless
                );
            }
            Ok(false) => {
                warned.remove(&format!("{id}:cursor-mcp"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:cursor-mcp")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: ~/.cursor/mcp.json MCP reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: ~/.cursor/mcp.json MCP reconcile still failing: {e:#}");
                }
            }
        }

        // The activity probe: `~/.rmng/hook.py` plus its registration under `.hooks` in
        // `~/.claude/settings.json`. What tells working from stuck (see `crate::stuck`).
        // Stamped on a hash of the script, so editing it re-pushes fleet-wide by itself.
        match ensure_claude_hook(app, id).await {
            Ok(true) => {
                warned.remove(&format!("{id}:claude-hook"));
                tracing::info!(
                    target: "clone_reconcile",
                    "clone {id}: installed the activity probe and registered its hooks"
                );
            }
            Ok(false) => {
                warned.remove(&format!("{id}:claude-hook"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:claude-hook")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: activity probe install failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: activity probe install still failing: {e:#}");
                }
            }
        }

        // Codex's `~/.codex/config.toml` MCP tables. A MERGE, not a rewrite: everything else in
        // that file is the operator's (model, approval_policy, sandbox, their own MCP servers).
        match ensure_codex_mcp(app, id, h.headless).await {
            Ok(true) => {
                warned.remove(&format!("{id}:codex-mcp"));
                tracing::info!(
                    target: "clone_reconcile",
                    "clone {id}: merged ~/.codex/config.toml MCP servers (headless={})",
                    h.headless
                );
            }
            Ok(false) => {
                warned.remove(&format!("{id}:codex-mcp"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:codex-mcp")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: ~/.codex/config.toml MCP merge failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: ~/.codex/config.toml MCP merge still failing: {e:#}");
                }
            }
        }

        match ensure_payload_current(app, id, h.headless).await {
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

/// Post-op convergence: run the full chain for one clone in the background after
/// fork/rebase/migrate/unarchive complete. The clone may not be running yet (rebase ends
/// stopped; migration restarts the fleet after the window), so this waits — bounded —
/// for it to come up instead of assuming the op left it running. Fire-and-forget with
/// logging: a failure surfaces in the warn log, and the next boot pass or Settings save
/// retries. Replaces what the 30 s loop used to guarantee for these transitions.
pub fn spawn_converge_after_start(app: &App, id: &str, why: &str) {
    let app = app.clone();
    let id = id.to_string();
    let why = why.to_string();
    tokio::spawn(async move {
        // Poll for the container: 10 s cadence, 30 min cap. A clone that never comes up
        // (still archived, deleted mid-wait) exits quietly — its next start re-triggers.
        for _ in 0..180 {
            let row = app.store.get().hosts.into_iter().find(|h| h.id == id);
            let Some(h) = row else { return };
            if !h.managed || !is_safe_id(&h.id) {
                return;
            }
            if app.docker.is_running(&id).await.unwrap_or(false) {
                let mut warned = HashSet::new();
                sync_clone_contents(&app, &h, &mut warned).await;
                tracing::info!(target: "clone_reconcile", "post-{why} sync converged {id}");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
        tracing::warn!(target: "clone_reconcile", "post-{why} sync gave up waiting for {id} to start");
    });
}

/// Run the full content chain over every running managed clone: the boot pass, the
/// Settings-save fan-out, and (single-clone, via the hosts filter at the call site)
/// post-op convergence share this one entry point.
pub async fn sync_all_running(app: &App, reason: &str) {
    let hosts: Vec<_> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived && is_safe_id(&h.id))
        .collect();
    let mut warned = HashSet::new();
    let mut n = 0;
    for h in &hosts {
        sync_clone_contents(app, h, &mut warned).await;
        n += 1;
    }
    tracing::info!(target: "clone_reconcile", "sync-all ({reason}): converged {n} clones");
}

/// Convergence entry point, run once at server start: a single full pass over every
/// running managed clone, so upgrades (payload, probe, MCP sets) land without waiting
/// on any timer. There is no polling loop anymore — after boot, convergence rides
/// explicit triggers only: the pre-boot tar (create), Settings-save fan-out, and
/// post-op sync after fork/rebase/migrate/unarchive.
pub async fn run(app: App) {
    sync_all_running(&app, "boot").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prepare script creates the dirs the `authorized_keys` upload needs and NOTHING else.
    /// It must never delete or rewrite a file under `~/.ssh`: those are the user's now, including
    /// `id_ed25519` (which this script used to remove when it matched the retired fleet key) and
    /// `config`.
    #[test]
    fn ssh_prepare_script_only_creates_dirs() {
        let s = ssh_prepare_script();
        assert!(
            s.contains("install -d -o rmng -g rmng -m700 /home/rmng/.ssh"),
            "{s}"
        );
        assert!(s.contains("mkdir -p /etc/ssh"), "{s}");
        // No destructive verb anywhere, and no reference to a user-owned file.
        for banned in [
            "rm -f",
            "rm ",
            "pkill",
            "id_ed25519",
            "/home/rmng/.ssh/config",
            "fleet",
        ] {
            assert!(
                !s.contains(banned),
                "prepare script must not mention {banned:?}:\n{s}"
            );
        }
    }

    /// A stream tag is compared against the constant, never a spelled-out literal.
    ///
    /// `exec_script` tags stdout `"out"`. `exec_ok_marked` compared against `"stdout"`, so it
    /// never saw its marker and always returned false — meaning the agent-wrapper was never
    /// restarted after `/etc/environment` changed, which is the one thing the marker exists to
    /// trigger. It failed silently for months: the shell-level tests below run the script
    /// directly and grep its stdout themselves, so they never exercised the tag at all.
    #[test]
    fn a_stream_tag_is_never_compared_against_a_bare_literal() {
        let src = include_str!("clone_reconcile.rs");
        let body = &src[..src.find("mod tests").unwrap_or(src.len())];
        assert!(
            !body.contains(r#"stream == ""#),
            "compare against crate::docker::STREAM_OUT, so a wrong spelling cannot compile"
        );
    }

    /// NOTHING the reconciler runs may delete a clone's provider credential files.
    ///
    /// The group-proxy era had a step that did exactly that (`dead_creds_cleanup_script`) —
    /// correct then, because the proxy owned tokens and a clone must not carry its own. Under
    /// the restored model those two files ARE the auth: `claude::apply_clone_token` and
    /// `codex::apply_clone_token` write them, and both agents re-read them per request.
    ///
    /// It survived the revert and ran on every pass, ~30 s after each clone was created. The
    /// failure was invisible: the create op logged `account: assigned …` and went green, the
    /// clone row kept showing the account, and `push_stale_tokens` would NOT repair it — its
    /// in-memory `pushed` map already recorded that exact token as delivered, so the clone
    /// stayed tokenless until the account's token next rotated or the server restarted.
    ///
    /// This test greps the emitted scripts rather than asserting a function is absent, so it
    /// also catches the deletion being reintroduced somewhere else in the file.
    /// The Codex MCP merge must leave everything that is not a managed table alone.
    ///
    /// `~/.codex/config.toml` is the operator's file: `model`, `approval_policy`,
    /// `sandbox_*`, `[profiles.*]`, and their own `[mcp_servers.*]` all live there. It used to
    /// Every parent directory the parity tar writes into must be pre-created by the
    /// template with the clone user's owner.
    ///
    /// Docker's tar extract invents a missing parent as root:root, and the agent then cannot
    /// write beside the file we placed. That is exactly what happened when `~/.pi/agent/AGENTS.md`
    /// was added without the matching `install -d`: the wrapper could not write its MCP tool
    /// cache, so the desktop tools were never promoted and every session ran proxy-only.
    /// The directory source of truth is phase 30 — this test reads that script, so removing
    /// a dir there fails here instead of in a clone.
    const TEMPLATE_PHASE_30: &str = include_str!("../../../template/setup/30-user.sh");

    #[test]
    fn the_template_owns_every_parity_parent_dir() {
        for entry in codex_parity_entries(false, "prompt") {
            let parent = std::path::Path::new(&entry.path)
                .parent()
                .expect("entry has a parent")
                .to_string_lossy()
                .to_string();
            // Template uses $USERNAME, so match the stable suffix, not the home path.
            let absolute = format!("/{parent}");
            let suffix = absolute
                .strip_prefix("/home/rmng")
                .expect("parity entry lives under the clone home");
            assert!(
                TEMPLATE_PHASE_30.contains(suffix),
                "phase 30 never creates *{suffix}, so the tar extract would make it root-owned",
            );
        }
    }

    /// Adding a directory to the prepare script has to re-stamp, or clones already stamped for
    /// this content keep the broken ownership forever.
    #[test]
    fn the_codex_parity_stamp_tracks_the_prepare_script() {
        let entries = codex_parity_entries(false, "prompt");
        let mut h = std::collections::hash_map::DefaultHasher::new();
        desired_payload_hash(&entries).hash(&mut h);
        "a different prepare script".hash(&mut h);
        assert_ne!(
            codex_parity_desired(&entries),
            format!("{:016x}", h.finish()),
            "the stamp ignores the prepare script",
        );
    }

    /// be rewritten wholesale every reconcile pass, silently reverting any hand-edit within
    /// ~30 s. The merge is pure Rust now, so this runs the REAL merge function against a
    /// real file rather than asserting on generated text.
    #[test]
    fn codex_mcp_merge_preserves_user_settings() {
        let dir = std::env::temp_dir().join(format!("rmng-codexmcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let codex = dir.join("codex");
        std::fs::create_dir_all(&codex).unwrap();
        let cfg = codex.join("config.toml");

        // A hand-customized file: settings before AND after the managed tables, a user's own
        // MCP server, and a table whose name merely starts the same way.
        let original = "# my own settings\n\
             model_reasoning_effort = \"high\"\n\
             approval_policy = \"never\"\n\
             \n\
             [sandbox_workspace_write]\n\
             network_access = true\n\
             \n\
             [mcp_servers.desktop]\n\
             url = \"http://127.0.0.1:9004\"\n\
             \n\
             [mcp_servers.my_own]\n\
             url = \"http://localhost:7777\"\n\
             \n\
             [profiles.fast]\n\
             model = \"gpt-5.5\"\n";
        std::fs::write(&cfg, original).unwrap();

        let run = |headless: bool| {
            // Missing file merges from empty, like the merge does on a fresh clone.
            let current = std::fs::read_to_string(&cfg).unwrap_or_default();
            let merged = merge_codex_config(&current, headless);
            std::fs::write(&cfg, &merged).unwrap();
            merged
        };

        let body = run(false);
        // Every operator-owned line survives, wherever it sat relative to the managed tables.
        for keep in [
            "# my own settings",
            "model_reasoning_effort = \"high\"",
            "approval_policy = \"never\"",
            "[sandbox_workspace_write]",
            "network_access = true",
            "[mcp_servers.my_own]",
            "url = \"http://localhost:7777\"",
            "[profiles.fast]",
        ] {
            assert!(body.contains(keep), "merge dropped {keep:?}:\n{body}");
        }
        // The managed tables are present exactly once — not duplicated by the re-append.
        assert_eq!(body.matches("[mcp_servers.desktop]").count(), 1, "{body}");
        assert_eq!(body.matches("[mcp_servers.linear]").count(), 1, "{body}");
        assert!(body.contains("bearer_token_env_var = \"LINEAR_API_KEY\""));

        // Idempotent: a converged clone must not churn the file every pass.
        assert_eq!(run(false), body, "second identical pass rewrote the file");

        // Headless never renders `desktop`, but it no longer deletes one either: the merge
        // is set-only, and headless is immutable per clone so a headless file never holds
        // the table in the first place.
        let hl = run(true);
        assert!(
            hl.contains("[mcp_servers.desktop]"),
            "set-only merge must leave existing tables alone:\n{hl}"
        );
        assert!(hl.contains("[mcp_servers.linear]"));
        assert!(
            hl.contains("[mcp_servers.my_own]"),
            "headless dropped the user's own server"
        );
        assert!(hl.contains("model_reasoning_effort = \"high\""));

        // ...while a headless file that never had it stays without it.
        std::fs::write(&cfg, "# fresh\n").unwrap();
        let hl_fresh = run(true);
        assert!(!hl_fresh.contains("[mcp_servers.desktop]"));
        assert!(hl_fresh.contains("[mcp_servers.linear]"));

        // Tables from older servers pass through untouched now: the merge is set-only and
        // there are no stale clones to heal. (If one ever surfaces, its dead wiring is the
        // operator's to clear — the merge will not touch it either way.)
        // The group-proxy era's dead wiring passes through like anything else. A merge that
        // only replaces what it currently emits never removes what it used to — the same trap
        // a retired-keys list once solved. `model_provider = "rmng"` beats the `~/.codex/auth.json`
        // the server writes, and its `base_url` is a route that now 404s, so leaving these behind
        // means Codex is authenticated and still broken. This body is a real production clone's.
        std::fs::write(
            &cfg,
            "# Managed by RMNG. Re-created by the RMNG clone reconciler.\n\
             \n\
             model_provider = \"rmng\"\n\
             model = \"gpt-5.6-terra\"\n\
             model_reasoning_effort = \"high\"\n\
             \n\
             [mcp_servers.desktop]\n\
             url = \"http://127.0.0.1:9004\"\n\
             \n\
             [model_providers.rmng]\n\
             name = \"RMNG\"\n\
             base_url = \"http://rmng-control:9000/cc/v1\"\n\
             env_key = \"RMNG_PROXY_KEY\"\n\
             \n\
             [profiles.fast]\n\
             model = \"gpt-5.5\"\n",
        )
        .unwrap();
        let cleaned = run(false);
        // Pass-through: old tables and keys survive the merge byte-for-byte.
        for kept in [
            "[model_providers.rmng]",
            "base_url = \"http://rmng-control:9000/cc/v1\"",
            "model_provider = \"rmng\"",
            "model = \"gpt-5.6-terra\"",
            "model_reasoning_effort = \"high\"",
            "[profiles.fast]",
            "model = \"gpt-5.5\"",
        ] {
            assert!(cleaned.contains(kept), "merge dropped {kept:?}:\n{cleaned}");
        }
        // ...and the managed tables are still refreshed in place.
        assert_eq!(cleaned.matches("[mcp_servers.desktop]").count(), 1);

        // A clone with no config.toml at all gets a valid one rather than an error.
        std::fs::remove_file(&cfg).unwrap();
        let fresh = run(false);
        assert!(fresh.contains("[mcp_servers.linear]"));
        assert!(fresh.contains("[mcp_servers.desktop]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_reconcile_script_removes_provider_credentials() {
        // The credential and MCP merges are pure-Rust home writes now (no guest scripts
        // left that could touch auth files); the remaining exec scripts are covered here.
        let scripts: Vec<(&str, String)> = vec![
            ("ssh_bootstrap", ssh_bootstrap_script().to_string()),
            ("etc_environment_sync", etc_environment_sync_script("A=1\n")),
        ];
        for (name, body) in scripts {
            for cred in [".claude/.credentials.json", ".codex/auth.json"] {
                assert!(
                    !body.contains(cred),
                    "{name} touches {cred} — that file IS the clone's auth under the restored \
                     credential-injection model, and deleting it fails silently"
                );
            }
        }
    }

    #[test]
    fn codex_parity_entries_install_global_guidance_and_linear_mcp() {
        let prompt = "# House rules\n\nBe excellent. SENTINEL-A+C.\n";
        let entries = codex_parity_entries(false, prompt);
        // The SAME global prompt body lands in both agents' native rules files.
        for path in ["home/rmng/.claude/CLAUDE.md", "home/rmng/.codex/AGENTS.md"] {
            let e = entries
                .iter()
                .find(|e| e.path == path)
                .unwrap_or_else(|| panic!("missing {path}"));
            assert_eq!(e.mode, 0o644);
            assert_eq!((e.uid, e.gid), (1000, 1000));
            assert_eq!(String::from_utf8(e.data.clone()).unwrap(), prompt);
        }
        // Cursor takes the same body, wrapped as an always-applied rule, because it reads
        // neither of the files above.
        let rule = entries
            .iter()
            .find(|e| e.path == "home/rmng/.cursor/rules/rmng.mdc")
            .expect("missing Cursor rule");
        assert_eq!(rule.mode, 0o644);
        assert_eq!((rule.uid, rule.gid), (1000, 1000));
        let body = String::from_utf8(rule.data.clone()).unwrap();
        assert!(body.starts_with("---\n"), "front matter first: {body}");
        assert!(
            body.contains("\nalwaysApply: true\n"),
            "unconditional: {body}"
        );
        assert!(
            body.ends_with(prompt),
            "the prompt is the body, verbatim: {body}"
        );
        // And the directory it lands in is made ahead of it (phase 30), or tar creates
        // it root-owned.
        assert!(TEMPLATE_PHASE_30.contains("/.cursor/rules"));

        // The node-agent MCP descriptor is part of the bundle.
        let desc = entries
            .iter()
            .find(|e| e.path == "home/rmng/.config/rmng/mcp.json")
            .expect("missing mcp.json descriptor");
        assert!(
            String::from_utf8(desc.data.clone())
                .unwrap()
                .contains("\"linear\"")
        );
        let agents = entries
            .iter()
            .find(|e| e.path == "home/rmng/.codex/AGENTS.md")
            .expect("missing Codex AGENTS.md");
        let agents_body = String::from_utf8(agents.data.clone()).unwrap();
        assert!(agents_body.contains("SENTINEL-A+C"));

        // `~/.codex/config.toml` is deliberately NOT in this set: it is merged in place by
        // `merge_codex_config` so the operator's own settings survive, not shipped as a tar
        // entry that would overwrite the file. Shipping it here again would silently reintroduce
        // the clobber.
        assert!(
            !entries
                .iter()
                .any(|e| e.path == "home/rmng/.codex/config.toml"),
            "config.toml must be merged, never overwritten by the parity tar"
        );
        let managed = codex_mcp_toml(false);
        assert!(managed.contains("[mcp_servers.desktop]"));
        assert!(managed.contains("url = \"http://127.0.0.1:9004\""));
        assert!(managed.contains("[mcp_servers.linear]"));
        assert!(managed.contains("url = \"https://mcp.linear.app/mcp\""));
        assert!(managed.contains("bearer_token_env_var = \"LINEAR_API_KEY\""));
        // No provider block: Codex authenticates from ~/.codex/auth.json, not a base_url.
        assert!(!managed.contains("base_url"));
    }

    #[test]
    fn claude_mcp_merge_sets_desktop_headed_and_skips_it_headless() {
        let base = serde_json::json!({"projects": {"/x": {}}, "mcpServers": {}});
        let headed = merge_claude_mcp(&base, false).unwrap();
        assert_eq!(headed["mcpServers"]["linear"]["url"], "https://mcp.linear.app/mcp");
        assert_eq!(
            headed["mcpServers"]["desktop"]["url"],
            "http://127.0.0.1:9004"
        );
        // Operator state in the file survives the merge.
        assert_eq!(headed["projects"], serde_json::json!({"/x": {}}));

        // Headless skips desktop without touching it: nothing writes it, so a headless
        // file simply never contains it — and a hand-added one is left alone, not deleted.
        let headless = merge_claude_mcp(&serde_json::json!({}), true).unwrap();
        assert!(headless["mcpServers"].get("desktop").is_none());
        assert!(headless["mcpServers"].get("linear").is_some());
        let untouched = merge_claude_mcp(&headed, true).unwrap();
        assert_eq!(untouched["mcpServers"]["desktop"]["url"], "http://127.0.0.1:9004");

        // ${LINEAR_API_KEY} must be stored literally so Claude Code expands it from the session
        // env at runtime.
        assert_eq!(
            headed["mcpServers"]["linear"]["headers"]["Authorization"],
            "Bearer ${LINEAR_API_KEY}"
        );

        // A non-object base is a hard error.
        assert!(merge_claude_mcp(&serde_json::json!([1, 2]), false).is_err());

        // The stamp value tracks the headless bit so the reconciler re-applies on a state change.
        assert_ne!(claude_mcp_desired(false), claude_mcp_desired(true));
    }

    #[test]
    fn agent_configs_omit_desktop_mcp_when_headless() {
        // Headless clones have no desktop (the clone-daemon on :9004 is deleted), so the shared
        // `desktop` MCP is never rendered into any generated agent config while `linear` stays.
        let codex = codex_mcp_toml(true);
        assert!(!codex.contains("[mcp_servers.desktop]"));
        assert!(!codex.contains("127.0.0.1:9004"));
        assert!(codex.contains("[mcp_servers.linear]"));

        // Headed keeps desktop.
        let codex_headed = codex_mcp_toml(false);
        assert!(codex_headed.contains("[mcp_servers.desktop]"));
        assert!(codex_headed.contains("127.0.0.1:9004"));

        // The node-agent descriptor and the Claude merge agree with it.
        let hl = merge_claude_mcp(&serde_json::json!({}), true).unwrap();
        assert!(hl["mcpServers"].get("desktop").is_none());
        let desc_hl: serde_json::Value = serde_json::from_str(&mcp_descriptor_json(true)).unwrap();
        assert_eq!(desc_hl.as_array().unwrap().len(), 1);
        assert_eq!(desc_hl[0]["name"], "linear");
    }

    #[test]
    fn rmng_cli_skill_written_to_both_skill_locations() {
        let entries = codex_parity_entries(false, "guide");
        for path in [
            "home/rmng/.claude/skills/rmng-cli/SKILL.md",
            "home/rmng/.agents/skills/rmng-cli/SKILL.md",
        ] {
            let e = entries
                .iter()
                .find(|e| e.path == path)
                .unwrap_or_else(|| panic!("missing {path}"));
            assert_eq!(e.mode, 0o644);
            assert_eq!((e.uid, e.gid), (1000, 1000));
            let body = String::from_utf8(e.data.clone()).unwrap();
            assert!(
                body.starts_with("---\nname: rmng-cli\n"),
                "SKILL.md needs skill frontmatter"
            );
            let description = body
                .lines()
                .find(|line| line.starts_with("description: "))
                .expect("SKILL.md needs a description");
            assert!(
                description.starts_with("description: \""),
                "SKILL.md description must quote YAML punctuation"
            );
            assert!(body.contains("rmng clone ls") && body.contains("rmng clone exec"));
            // A flag the CLI takes and the skill omits is a flag no agent in a clone will ever
            // use. These three are the ones that make a delegating session's history readable.
            for flag in ["--sidechain", "--no-sidechain", "--agent <id>"] {
                assert!(body.contains(flag), "the ledger section has to name {flag}");
            }
        }
        // Phase 30 creates both skill directories.
        assert!(TEMPLATE_PHASE_30.contains("/.claude/skills/rmng-cli"));
        assert!(TEMPLATE_PHASE_30.contains("/.agents/skills/rmng-cli"));
    }

    /// The create path renders merge-owned files by running the merges on an empty base
    /// (single code path for create and converge — no separate initial renderers). The
    /// template bakes none of the targets, so on a fresh clone the base is always empty.
    #[test]
    fn preboot_files_are_the_merges_on_empty_base() {
        // ~/.claude.json: headed gets both servers, headless skips desktop.
        let v = merge_claude_mcp(&serde_json::json!({}), false).unwrap();
        assert_eq!(v["mcpServers"]["linear"]["url"], "https://mcp.linear.app/mcp");
        assert_eq!(
            v["mcpServers"]["linear"]["headers"]["Authorization"],
            "Bearer ${LINEAR_API_KEY}"
        );
        assert_eq!(
            v["mcpServers"]["desktop"]["url"],
            "http://127.0.0.1:9004"
        );
        let v = merge_claude_mcp(&serde_json::json!({}), true).unwrap();
        assert!(v["mcpServers"].get("desktop").is_none());
        assert!(v["mcpServers"].get("linear").is_some());
        // ~/.cursor/mcp.json: merge sets exactly the wanted servers.
        let v = merge_cursor_mcp(&serde_json::json!({}), false, "lin_key").unwrap();
        assert_eq!(v["mcpServers"], cursor_mcp_want(false, "lin_key"));
        // ~/.codex/config.toml: merge renders the managed tables onto nothing.
        let toml = codex_mcp_toml(false);
        assert!(toml.contains("[mcp_servers.desktop]") && toml.contains("[mcp_servers.linear]"));
        assert!(!codex_mcp_toml(true).contains("desktop"));
        assert_eq!(merge_codex_config("", false), toml);
        // Hook registrations: whole-`.hooks` assignment on `{}` (+ version for Cursor).
        let v: serde_json::Value = serde_json::from_str(&claude_settings_initial()).unwrap();
        assert_eq!(v["hooks"].as_object().unwrap().len(), HOOK_EVENTS.len());
        for event in HOOK_EVENTS {
            let cmd = v["hooks"][event][0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, HOOK_IN_CLONE, "{event} points at the probe");
        }
        let v: serde_json::Value = serde_json::from_str(&cursor_hooks_initial()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(
            v["hooks"].as_object().unwrap().len(),
            CURSOR_HOOK_EVENTS.len()
        );
    }

    #[test]
    fn managed_mcp_is_the_single_source_for_all_emitters() {
        // Headed: every emitter renders both managed servers with the right auth form.
        let codex = codex_mcp_toml(false);
        assert!(codex.contains("[mcp_servers.desktop]") && codex.contains("http://127.0.0.1:9004"));
        assert!(codex.contains("[mcp_servers.linear]"));
        assert!(codex.contains("bearer_token_env_var = \"LINEAR_API_KEY\""));

        let merged = merge_claude_mcp(&serde_json::json!({}), false).unwrap();
        assert_eq!(
            merged["mcpServers"]["desktop"]["url"],
            "http://127.0.0.1:9004"
        );
        assert_eq!(
            merged["mcpServers"]["linear"]["headers"]["Authorization"],
            "Bearer ${LINEAR_API_KEY}"
        );

        // The node-agent descriptor: desktop carries alwaysLoad, linear carries bearerEnv.
        let desc: serde_json::Value = serde_json::from_str(&mcp_descriptor_json(false)).unwrap();
        let arr = desc.as_array().unwrap();
        let desktop = arr.iter().find(|s| s["name"] == "desktop").unwrap();
        let linear = arr.iter().find(|s| s["name"] == "linear").unwrap();
        assert_eq!(desktop["alwaysLoad"], true);
        assert_eq!(desktop["url"], "http://127.0.0.1:9004");
        assert_eq!(linear["bearerEnv"], "LINEAR_API_KEY");
        assert!(linear.get("alwaysLoad").is_none());

        // Headless: desktop is filtered out of every emitter; linear stays.
        assert!(!codex_mcp_toml(true).contains("desktop"));
        let merged_hl = merge_claude_mcp(&serde_json::json!({}), true).unwrap();
        assert!(merged_hl["mcpServers"].get("desktop").is_none());
        let desc_hl: serde_json::Value = serde_json::from_str(&mcp_descriptor_json(true)).unwrap();
        assert_eq!(desc_hl.as_array().unwrap().len(), 1);
        assert_eq!(desc_hl[0]["name"], "linear");
    }

    #[test]
    fn codex_parity_stamp_hash_changes_when_config_changes() {
        let original = codex_parity_stamp_entry_for(&codex_parity_entries(false, "guide"));
        // Any content change in the set must move the hash; AGENTS.md stands in for the file
        // that used to be edited here (config.toml, now merged in place rather than shipped).
        let mut changed = codex_parity_entries(false, "guide");
        changed
            .iter_mut()
            .find(|e| e.path == "home/rmng/.codex/AGENTS.md")
            .unwrap()
            .data
            .extend_from_slice(b"\n# changed\n");
        let updated = codex_parity_stamp_entry_for(&changed);

        assert_eq!(original.path, "etc/rmng/codex-parity-hash");
        assert_eq!(updated.path, "etc/rmng/codex-parity-hash");
        assert_ne!(original.data, updated.data);
    }

    #[test]
    fn etc_environment_sync_uses_desired_env_and_removes_legacy_environment_d() {
        let script = etc_environment_sync_script(
            "RMNG_CONTROL_URL=http://rmng-control:9000\nLINEAR_API_KEY=secret\n",
        );
        assert!(script.contains("base64 -d"));
        assert!(script.contains("/etc/environment"));
        assert!(script.contains("drop[$1]=1"));
        assert!(script.contains("awk '/^[A-Za-z_][A-Za-z0-9_]*=/' \"$desired\" >> \"$tmp\""));
        assert!(script.contains("cmp -s \"$tmp\" \"$etc\""));
        assert!(script.contains("install -m 0644"));
        assert!(script.contains("rm -f \"$legacy\""));
    }

    /// The agent-wrapper restart is gated on this script PRINTING the marker, and a wrapper
    /// that never restarts keeps a stale `ANTHROPIC_BASE_URL` forever (the bug this fixes)
    /// while one that restarts every pass interrupts chat twice a minute. Both failure modes
    /// live in shell, not Rust, so run the real script against a real file rather than
    /// asserting on its text.
    /// Run the real sync script against a temp `/etc/environment`, returning whether it
    /// announced a change. Redirects `$etc` (and parks the legacy path somewhere absent) so no
    /// root or container is needed.
    fn run_env_sync(dir: &std::path::Path, etc: &std::path::Path, desired: &str) -> bool {
        let script = etc_environment_sync_script(desired)
            .replace("etc=/etc/environment", &format!("etc={}", etc.display()))
            .replace(
                "legacy=/home/rmng/.config/environment.d/30-rmng-preset.conf",
                &format!("legacy={}/nonexistent-legacy", dir.display()),
            )
            .replace("install -m 0644 -o root -g root", "install -m 0644")
            .replace(
                "rmdir /home/rmng/.config/environment.d",
                "rmdir /nonexistent",
            );
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run env sync script");
        assert!(
            out.status.success(),
            "script failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).contains(ENV_CHANGED_MARKER)
    }

    /// The sync converges `/etc/environment` onto the desired keys while leaving
    /// operator-owned lines untouched. Gen-2 images never carried the old proxy-era
    /// inference wiring, so there is no retired-keys strip-list anymore: a key that is
    /// neither desired nor operator-owned stays as-is.
    #[test]
    fn env_sync_converges_desired_keys_but_keeps_operator_lines() {
        let dir =
            std::env::temp_dir().join(format!("rmng-envsync-converge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let etc = dir.join("environment");
        std::fs::write(
            &etc,
            "# operator's own notes\n\
             RMNG_PROXY_KEY=keepme\n\
             MY_OWN_VAR=hello\n\
             export EDITOR=vim\n",
        )
        .unwrap();

        let changed = run_env_sync(
            &dir,
            &etc,
            "RMNG_CONTROL_URL=http://rmng-control:9000\nRMNG_PROXY_KEY=keepme\n",
        );
        assert!(
            changed,
            "converging is a change; the agent-wrapper must restart"
        );
        let body = std::fs::read_to_string(&etc).unwrap();

        assert!(
            body.contains("RMNG_PROXY_KEY=keepme"),
            "identity key was dropped:\n{body}"
        );
        assert!(
            body.contains("RMNG_CONTROL_URL=http://rmng-control:9000"),
            "{body}"
        );
        // Operator-owned content is never touched.
        assert!(
            body.contains("# operator's own notes"),
            "comment lost:\n{body}"
        );
        assert!(
            body.contains("MY_OWN_VAR=hello"),
            "operator var lost:\n{body}"
        );
        assert!(
            body.contains("export EDITOR=vim"),
            "export line lost:\n{body}"
        );

        // Idempotent: a second pass with the same desired env is not a change.
        let changed = run_env_sync(
            &dir,
            &etc,
            "RMNG_CONTROL_URL=http://rmng-control:9000\nRMNG_PROXY_KEY=keepme\n",
        );
        assert!(
            !changed,
            "a converged clone must not restart its agent-wrapper every pass"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_sync_prints_the_marker_only_when_it_actually_rewrites() {
        let dir = std::env::temp_dir().join(format!("rmng-envsync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let etc = dir.join("environment");
        let run = |desired: &str| -> (String, bool) {
            let printed = run_env_sync(&dir, &etc, desired);
            (String::new(), printed)
        };

        // First write: the file did not exist, so this is a change.
        let (_, printed) = run("RMNG_CONTROL_URL=http://rmng-control:9000\n");
        assert!(printed, "first write must announce the change");
        assert!(
            std::fs::read_to_string(&etc)
                .unwrap()
                .contains("rmng-control:9000"),
            "the new value must land on disk"
        );

        // Identical desired env: no rewrite, so NO marker — this is what keeps the reconciler
        // from restarting the agent-wrapper on every 30 s pass.
        let (_, printed) = run("RMNG_CONTROL_URL=http://rmng-control:9000\n");
        assert!(!printed, "an unchanged env must not announce a change");

        // A real change announces again.
        let (_, printed) = run("RMNG_CONTROL_URL=http://rmng-control:9000\nEXTRA=1\n");
        assert!(printed, "a changed env must announce it");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restarting the holder is what resets every window position, so the payload refresh only
    /// enables it and leaves starting it to the daemon.
    #[test]
    fn the_payload_restart_enables_the_holder_without_touching_a_live_one() {
        let script = restart_clone_daemon_script("1920x1080+0+0*");
        assert!(script.contains("systemctl --user enable rmng-session-holder.service"));
        assert!(
            !script.contains("restart rmng-session-holder"),
            "the reconcile pass must not restart the session holder:\n{script}"
        );
        assert!(
            !script.contains("start rmng-session-holder"),
            "starting the holder here races the outgoing daemon's monitors:\n{script}"
        );
        // The daemon still restarts, and still only when its unit exists and is unmasked
        // (headless clones mask it, and an unconditional restart would abort the whole
        // reconcile there).
        assert!(script.contains("systemctl --user restart rmng-clone-daemon.service"));
        assert!(script.contains("cat rmng-clone-daemon.service"));
        assert!(script.contains("readlink"));
    }

    /// A clone that has never run a holder gets the active preset written into the holder's
    /// memory before the daemon starts it. Without it the first session comes up on the
    /// built-in single 1920x1080, which is how a whole fleet once landed on a layout nobody
    /// chose. A clone that already remembers a layout must be left alone: that memory is the
    /// layout it was last viewed with.
    #[test]
    fn the_payload_restart_seeds_a_layout_only_when_the_holder_has_none() {
        let script = restart_clone_daemon_script("2560x1440+2560+0*,2560x1440+0+0");
        assert!(
            script.contains("[ ! -e /home/rmng/.rmng/monitors ]"),
            "the seed must be guarded on the file being absent:\n{script}"
        );
        assert!(script.contains("'2560x1440+2560+0*,2560x1440+0+0' > /home/rmng/.rmng/monitors"));
        assert!(script.contains("chown rmng:rmng /home/rmng/.rmng/monitors"));
        // Before the restart, because the restart is what makes the daemon start the holder.
        let seed = script.find(".rmng/monitors").expect("seeds a layout");
        let restart = script
            .find("restart rmng-clone-daemon")
            .expect("restarts the daemon");
        assert!(
            seed < restart,
            "the seed has to land before the daemon starts the holder"
        );
    }

    /// The seed is written in the same `WxH+X+Y[*]` form the holder's own layout memory uses,
    /// so one parser reads both.
    #[test]
    fn a_layout_is_written_the_way_the_holder_reads_it() {
        let mons = vec![
            wire::MonitorSpec {
                width: 2560,
                height: 1440,
                x: 2560,
                y: 0,
                primary: true,
            },
            wire::MonitorSpec {
                width: 2560,
                height: 1440,
                x: 0,
                y: 0,
                primary: false,
            },
        ];
        assert_eq!(monitors_csv(&mons), "2560x1440+2560+0*,2560x1440+0+0");
        assert_eq!(monitors_csv(&[]), "");
    }

    /// A headless clone has no Mutter for the holder to hold, and its unit would restart-loop
    /// against a desktop that is never coming up.
    #[test]
    fn the_holder_unit_entry_lands_with_clone_ownership() {
        let entry = session_holder_unit_entry();
        assert_eq!(
            entry.path,
            "home/rmng/.config/systemd/user/rmng-session-holder.service"
        );
        assert_eq!(
            (entry.uid, entry.gid, entry.mode),
            (CLONE_UID, CLONE_GID, 0o644)
        );
        let body = String::from_utf8(entry.data).unwrap();
        assert!(body.contains("ExecStart=/opt/rmng/bin/rmng-clone-daemon --session-holder"));
        // No baked layout: the holder boots on the one it remembers in ~/.rmng/monitors.
        assert!(
            !body.contains("RMNG_MONITORS"),
            "the shipped unit must not bake a layout"
        );
    }

    #[test]
    fn desired_payload_hash_changes_when_payload_bytes_change() {
        let hash_of = |path: &str, data: &[u8]| {
            desired_payload_hash(&[TarEntry {
                path: path.into(),
                data: data.to_vec(),
                mode: 0o755,
                uid: 0,
                gid: 0,
            }])
        };
        assert_ne!(
            hash_of("opt/rmng/bin/rmng-clone-daemon", b"old"),
            hash_of("opt/rmng/bin/rmng-clone-daemon", b"new")
        );
        assert_ne!(
            hash_of("opt/rmng/bin/agent-wrapper", b"same"),
            hash_of("usr/local/bin/rmng", b"same")
        );
    }
}

#[cfg(test)]
mod hook_tests {
    use super::*;

    /// Run the real merge functions against real files, the way the Codex and
    /// `/etc/environment` merges are tested. The merges are pure Rust now, so no shell
    /// is involved — same behavioral coverage, minus the bash.
    fn run(settings: &std::path::Path) -> String {
        run_both(settings).0
    }

    /// Both files the merge touches: Claude Code's settings and Cursor's hooks.
    fn run_both(settings: &std::path::Path) -> (String, String) {
        let cursor = settings.with_file_name("cursor-hooks.json");
        let read_base = |path: &std::path::Path| -> serde_json::Value {
            let raw = std::fs::read(path).unwrap_or_default();
            if raw.iter().all(|b| b.is_ascii_whitespace()) {
                serde_json::json!({})
            } else {
                serde_json::from_slice(&raw).unwrap()
            }
        };
        let merged_settings = merge_claude_hooks(&read_base(settings)).unwrap();
        std::fs::write(settings, merged_settings.to_string()).unwrap();
        let merged_cursor = merge_cursor_hooks(&read_base(&cursor)).unwrap();
        std::fs::write(&cursor, merged_cursor.to_string()).unwrap();
        (
            std::fs::read_to_string(settings).unwrap(),
            std::fs::read_to_string(&cursor).unwrap(),
        )
    }

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rmng-hook-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn registration_keeps_every_key_the_operator_owns() {
        let dir = tmpdir("merge");
        let settings = dir.join("settings.json");
        // What a real clone's file holds: Claude Code's own user state, none of it ours.
        std::fs::write(
            &settings,
            r#"{"model":"opus[1m]","effortLevel":"xhigh","theme":"auto",
                "skipDangerousModePermissionPrompt":true,"enabledPlugins":{"a":true}}"#,
        )
        .unwrap();

        let body = run(&settings);
        let got: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(got["model"], "opus[1m]");
        assert_eq!(got["effortLevel"], "xhigh");
        assert_eq!(got["theme"], "auto");
        assert_eq!(got["skipDangerousModePermissionPrompt"], true);
        assert_eq!(got["enabledPlugins"]["a"], true);

        for event in HOOK_EVENTS {
            assert_eq!(
                got["hooks"][event][0]["hooks"][0]["command"], HOOK_IN_CLONE,
                "{event} must run the in-clone path, never this host's view of it"
            );
        }
        assert_eq!(got["hooks"].as_object().unwrap().len(), HOOK_EVENTS.len());

        // A second identical pass must not rewrite the file, or the stamp is the only thing
        // stopping an endless churn of settings writes at every clone.
        assert_eq!(
            run(&settings),
            body,
            "second identical pass rewrote the file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_empty_settings_file_is_created_rather_than_fatal() {
        let dir = tmpdir("absent");
        let settings = dir.join("settings.json");
        let got: serde_json::Value = serde_json::from_str(&run(&settings)).unwrap();
        assert_eq!(
            got["hooks"]["Stop"][0]["hooks"][0]["command"],
            HOOK_IN_CLONE
        );

        std::fs::write(&settings, "").unwrap();
        let got: serde_json::Value = serde_json::from_str(&run(&settings)).unwrap();
        assert_eq!(
            got["hooks"]["Stop"][0]["hooks"][0]["command"],
            HOOK_IN_CLONE
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_event_we_stop_emitting_stops_firing() {
        let dir = tmpdir("retire");
        let settings = dir.join("settings.json");
        // A registration from an older server, naming an event this one no longer emits.
        std::fs::write(
            &settings,
            r#"{"theme":"auto","hooks":{"Retired":[{"hooks":[{"type":"command","command":"/gone.py"}]}]}}"#,
        )
        .unwrap();
        let got: serde_json::Value = serde_json::from_str(&run(&settings)).unwrap();
        assert!(
            got["hooks"]["Retired"].is_null(),
            "assigning .hooks wholesale is what retires an event; merging into it would leave \
             /gone.py firing forever"
        );
        assert_eq!(got["theme"], "auto", "the operator's keys still survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stamp_tracks_the_probe_and_its_registration() {
        // Editing the Python must re-push without anyone remembering to bump a version.
        let base = claude_hook_desired();
        assert_eq!(base, claude_hook_desired(), "the hash must be stable");
        assert!(!base.is_empty());

        let mut entries = rmng_hook_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "home/rmng/.rmng/hook.py");
        // uid 1000, or the clone's own agent cannot write the log it is told to append to.
        assert_eq!((entries[0].uid, entries[0].gid), (CLONE_UID, CLONE_GID));
        assert_eq!(entries[0].mode, 0o755);

        entries[0].data.push(b'#');
        assert_ne!(
            desired_payload_hash(&entries),
            desired_payload_hash(&rmng_hook_entries())
        );
    }

    #[test]
    fn the_probe_is_valid_python_that_survives_junk_on_stdin() {
        let dir = tmpdir("py");
        let hook = dir.join("hook.py");
        std::fs::write(&hook, RMNG_HOOK_PY).unwrap();
        // A hook that exits non-zero interrupts the agent, so every path must exit 0 —
        // including a payload that is not even JSON.
        for stdin in [
            "",
            "not json at all",
            "[1,2,3]",
            r#"{"hook_event_name":"Stop"}"#,
        ] {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "printf %s {} | HOME={} python3 {}",
                    shell_quote(stdin),
                    dir.display(),
                    hook.display()
                ))
                .output()
                .expect("run the probe");
            assert!(
                out.status.success(),
                "stdin {stdin:?} made the probe exit {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        // The one well-formed payload above is the only line that should have been logged.
        let log = std::fs::read_to_string(dir.join(".rmng/agent-events.jsonl")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(
            lines.len(),
            4,
            "every invocation logs exactly one line: {log}"
        );
        let last: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(last["hook_event_name"], "Stop");
        assert!(last["ts"].as_f64().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cursor throws converting a Claude hook entry whose matcher is absent, and registers
    /// none of them when it does. This is the single key that keeps the probe alive there.
    #[test]
    fn every_claude_tool_hook_spells_out_its_matcher() {
        let dir = tmpdir("matcher");
        let settings = dir.join("settings.json");
        let got: serde_json::Value = serde_json::from_str(&run(&settings)).unwrap();
        for event in HOOK_EVENTS {
            assert_eq!(
                got["hooks"][event][0]["matcher"], "*",
                "{event} without a matcher makes Cursor drop every hook in the file"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cursor_gets_the_events_its_claude_converter_drops() {
        let dir = tmpdir("cursor");
        let settings = dir.join("settings.json");
        let (_, body) = run_both(&settings);
        let got: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(got["version"], 1);
        for event in CURSOR_HOOK_EVENTS {
            assert_eq!(
                got["hooks"][event][0]["command"], HOOK_IN_CLONE,
                "{event} must run the in-clone path"
            );
        }
        assert_eq!(
            got["hooks"].as_object().unwrap().len(),
            CURSOR_HOOK_EVENTS.len()
        );
        // The reason this file exists at all: Cursor's Claude converter has no name for
        // these, and the first is the only event a failed tool call fires.
        for missing in ["postToolUseFailure", "subagentStart"] {
            assert!(
                CURSOR_HOOK_EVENTS.contains(&missing),
                "{missing} is why the native registration exists"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cursors_own_hooks_survive_the_merge() {
        let dir = tmpdir("cursorkeep");
        let settings = dir.join("settings.json");
        let cursor = dir.join("cursor-hooks.json");
        std::fs::write(
            &cursor,
            r#"{"version":1,"hooks":{"beforeReadFile":[{"command":"/theirs.sh"}]}}"#,
        )
        .unwrap();

        let (_, body) = run_both(&settings);
        let got: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            got["hooks"]["beforeReadFile"].is_null(),
            "assigning .hooks wholesale is what retires an event, here as in the Claude file"
        );
        assert_eq!(got["hooks"]["stop"][0]["command"], HOOK_IN_CLONE);

        // A second identical pass must be byte-identical, or the stamp is all that stops an
        // endless churn of writes into a file Cursor watches and reloads on every change.
        assert_eq!(
            run_both(&settings).1,
            body,
            "second identical pass rewrote the file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Run the real Cursor MCP merge against a real file, for the same reason the hook merge
    /// runs its own: the merge is where this can be wrong, and asserting on text would not
    /// notice. The merge is pure Rust now, so no shell is involved.
    fn run_cursor_mcp(path: &std::path::Path, headless: bool, linear_key: &str) -> String {
        let current = std::fs::read(path).unwrap_or_default();
        let base: serde_json::Value = if current.iter().all(|b| b.is_ascii_whitespace()) {
            serde_json::json!({})
        } else {
            serde_json::from_slice(&current).unwrap()
        };
        let merged = merge_cursor_mcp(&base, headless, linear_key).unwrap();
        std::fs::write(path, merged.to_string()).unwrap();
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn cursor_gets_both_servers_with_the_bearer_already_resolved() {
        let dir = tmpdir("cursormcp");
        let path = dir.join("mcp.json");
        let body = run_cursor_mcp(&path, false, "lin_api_key_xyz");
        let got: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(got["mcpServers"]["desktop"]["url"], "http://127.0.0.1:9004");
        assert_eq!(
            got["mcpServers"]["linear"]["url"],
            "https://mcp.linear.app/mcp"
        );
        // Cursor expands nothing in this file, so an env reference would be sent verbatim as
        // the token and every Linear call would 401.
        assert_eq!(
            got["mcpServers"]["linear"]["headers"]["Authorization"],
            "Bearer lin_api_key_xyz"
        );
        assert!(
            !body.contains("${"),
            "no unexpanded reference may survive: {body}"
        );

        // A second identical pass must be byte-identical: Cursor watches this file and
        // reconnects every server when it changes.
        assert_eq!(run_cursor_mcp(&path, false, "lin_api_key_xyz"), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_headless_clone_skips_desktop_and_a_keyless_one_skips_linear() {
        let dir = tmpdir("cursordrop");
        let path = dir.join("mcp.json");

        // Headless never renders desktop (no daemon there); linear stays.
        let headless: serde_json::Value =
            serde_json::from_str(&run_cursor_mcp(&path, true, "lin_api_key_xyz")).unwrap();
        assert!(
            headless["mcpServers"]["desktop"].is_null(),
            "no daemon on a headless clone"
        );
        assert!(headless["mcpServers"]["linear"].is_object());

        // Keyless never renders linear (a headerless one would sit in Cursor's
        // "Needs attention" list forever) — but like every set-only merge it leaves an
        // existing entry alone rather than deleting it.
        let keyless_path = dir.join("keyless.json");
        let keyless: serde_json::Value =
            serde_json::from_str(&run_cursor_mcp(&keyless_path, false, "")).unwrap();
        assert!(keyless["mcpServers"]["desktop"].is_object());
        assert!(
            keyless["mcpServers"]["linear"].is_null(),
            "no key means no server"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_operators_own_cursor_servers_survive_the_merge() {
        let dir = tmpdir("cursorkeepmcp");
        let path = dir.join("mcp.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers":{"theirs":{"command":"npx","args":["-y","their-server"]}}}"#,
        )
        .unwrap();

        let got: serde_json::Value =
            serde_json::from_str(&run_cursor_mcp(&path, false, "k")).unwrap();
        assert_eq!(got["mcpServers"]["theirs"]["command"], "npx");
        assert_eq!(got["mcpServers"]["desktop"]["url"], "http://127.0.0.1:9004");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cursor_stamp_tracks_the_key_without_carrying_it() {
        let a = cursor_mcp_desired(false, "key-one");
        let b = cursor_mcp_desired(false, "key-two");
        assert_ne!(a, b, "a rotated key must re-apply");
        assert_eq!(
            a,
            cursor_mcp_desired(false, "key-one"),
            "and be stable otherwise"
        );
        assert_ne!(
            a,
            cursor_mcp_desired(true, "key-one"),
            "as must a headless flip"
        );
        for stamp in [a, b] {
            assert!(
                !stamp.contains("key-"),
                "the stamp file must not carry the key: {stamp}"
            );
        }
    }

    #[test]
    fn env_value_takes_the_last_duplicate() {
        let vars = vec![
            wire::EnvVar {
                key: "LINEAR_API_KEY".into(),
                value: "first".into(),
            },
            wire::EnvVar {
                key: "OTHER".into(),
                value: "x".into(),
            },
            wire::EnvVar {
                key: "LINEAR_API_KEY".into(),
                value: "second".into(),
            },
        ];
        assert_eq!(env_value(&vars, "LINEAR_API_KEY"), "second");
        assert_eq!(env_value(&vars, "ABSENT"), "");
    }

    fn shell_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', r#"'\''"#))
    }
}
