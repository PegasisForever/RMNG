//! Live migration for clones created by older control-server/template versions.
//!
//! New clones get current binaries and SSH material during `provision::clone_container`.
//! Existing running clones need an idempotent reconcile path so a control-server update can
//! make them operational without destructive recreate: install/enable clone-side sshd, refresh
//! injected payload binaries, then restart the clone daemon and agent wrapper so their running
//! processes use the current payload and configuration.

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

// ---- managed MCP servers: the single source of truth ---------------------------------
//
// The `desktop` + `linear` set every clone agent gets, defined ONCE here and rendered into
// each agent's own format by the emitters below (Claude `~/.claude.json` jq-merge, Codex
// `config.toml` and the neutral `~/.config/rmng/mcp.json` the
// node-agent reads). Change a URL / add a server here and all agents pick it up.

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
fn codex_mcp_toml(headless: bool) -> String {
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

/// The jq program body that reconciles `~/.claude.json`'s `.mcpServers` to the managed set:
/// each active server is SET, each inactive one is DELETED (so a headed→headless flip removes
/// `desktop`). linear's bearer is stored literally as `${LINEAR_API_KEY}` — Claude Code expands
/// it from the session env at runtime; single-quoted in the bash caller so bash won't expand it.
fn claude_mcp_jq_program(headless: bool) -> String {
    let mut steps: Vec<String> = Vec::new();
    for m in managed_mcp() {
        let path = format!(".mcpServers.{}", m.name);
        if headless && m.headless_only {
            steps.push(format!("del({path})"));
        } else {
            let obj = match m.bearer_env {
                Some(env) => format!(
                    r#"{{"type":"http","url":"{}","headers":{{"Authorization":"Bearer ${{{}}}"}}}}"#,
                    m.url, env
                ),
                None => format!(r#"{{"type":"http","url":"{}"}}"#, m.url),
            };
            steps.push(format!("{path} = {obj}"));
        }
    }
    steps.join(" | ")
}

/// The neutral MCP descriptor the node-agent reads (`~/.config/rmng/mcp.json`): a JSON array of
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
    serde_json::to_string_pretty(&serde_json::json!(servers)).unwrap_or_else(|_| "[]".into())
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

/// Current SSH-provisioning schema version. Bumped from the original `ok` when the fleet key
/// moved out of `~/.ssh` (gcr-ssh-agent crash fix + migration): an older clone stamped `ok`
/// no longer matches, so `ensure_ssh_ready` re-runs and applies the relocation + cleanup.
const SSH_STAMP_VERSION: &str = "v2";

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
/// (`jobs::run_clone`) and the per-clone resync below. Keeping one definition is what stops a
/// fresh clone from being born without the var and then having the reconciler add it up to
/// `RECONCILE_INTERVAL` later — a visible ~30 s window in which the clone's Claude Code ran on
/// its built-in default instead of ours.
pub(crate) fn claude_model_env_var() -> wire::EnvVar {
    wire::EnvVar {
        key: "ANTHROPIC_MODEL".into(),
        value: FALLBACK_CLAUDE_MODEL.to_string(),
    }
}

/// Env keys RMNG used to write into `/etc/environment` and no longer does.
///
/// [`etc_environment_sync_script`] builds its strip-list from the **desired** keys, so a key we
/// simply stop emitting is never removed from a clone that already has it — it would survive
/// forever. These four are the group-proxy era's inference wiring: `ANTHROPIC_BASE_URL` points at
/// the `rmng-cliproxy` container, which no longer exists, so a clone that kept it would fail every
/// agent request with no self-heal. Listing them here strips them (they are never re-appended),
/// and the resulting change trips [`ENV_CHANGED_MARKER`] → the agent-wrapper restart, which is the
/// only thing that can update an already-running wrapper's frozen process environment.
///
/// `RMNG_PROXY_KEY` is deliberately NOT here: it outlived the proxy as the clone's identity token
/// (sub-clone parent detection in `web.rs`, and clone↔clone SSH in the fleet CLI).
const RETIRED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
];

/// Merge the managed MCP tables into a clone's `~/.codex/config.toml`, preserving everything
/// else in the file.
///
/// This used to overwrite the file wholesale. It must not: `config.toml` is where a Codex user
/// puts their own settings (`model`, `approval_policy`, `sandbox_mode`, their own
/// `[mcp_servers.*]`), and a rewrite every reconcile pass reverted any hand-edit within ~30 s
/// with no warning. `~/.claude.json` already got the careful treatment (a jq merge, because it
/// is state-bearing); this is the same courtesy for the file Codex owns.
///
/// TOML has no jq, so the merge is a table-aware awk pass: it copies every line through, drops
/// exactly the `[mcp_servers.<managed>]` tables (from the header to the next table header or
/// EOF), and appends the freshly rendered ones. Nothing outside those tables is read or
/// rewritten — including a user's own `[mcp_servers.foo]`, which is not in the managed set and
/// so is copied verbatim.
///
/// Dropping-then-appending (rather than editing in place) is what makes a headed→headless flip
/// work: `desktop` is simply not in the appended set, and its old table was already removed.
/// TOML tables RMNG used to write into `~/.codex/config.toml` and no longer does, plus the bare
/// top-level keys that went with them.
///
/// The same trap as [`RETIRED_ENV_KEYS`], one file over: a merge that only replaces what it
/// currently emits never removes what it USED to emit. The group-proxy era pointed Codex at the
/// `/cc/v1` router with `model_provider = "rmng"` + a `[model_providers.rmng]` block; that route
/// now 404s, and an explicit `model_provider` beats the `~/.codex/auth.json` the server writes —
/// so leaving them behind means Codex is authenticated and still broken, on every clone, forever.
/// Verified against a real production clone: all six lines survived a merge that lacked this.
///
/// `model_reasoning_effort` is deliberately NOT here. It is a plain preference with no dead
/// endpoint behind it, and stripping it would be RMNG deleting an operator's setting rather than
/// cleaning up its own wiring.
const RETIRED_CODEX_TABLES: &[&str] = &["model_providers.rmng"];
const RETIRED_CODEX_KEYS: &[&str] = &["model_provider", "model"];

pub(crate) fn codex_mcp_merge_script(headless: bool) -> String {
    let managed: Vec<&str> = managed_mcp().iter().map(|m| m.name).collect();
    // `mcp_servers.desktop|mcp_servers.linear` — the tables this pass owns. Built from the
    // managed set, not the ACTIVE one, so a server dropped by the headless filter is still
    // recognised and removed rather than left orphaned.
    let owned = managed
        .iter()
        .map(|n| format!("mcp_servers.{n}"))
        .chain(RETIRED_CODEX_TABLES.iter().map(|t| (*t).to_string()))
        .collect::<Vec<_>>()
        .join("|");
    // Bare top-level keys (outside any table) RMNG used to set and now retires.
    let retired_keys = RETIRED_CODEX_KEYS.join("|");
    let desired = codex_mcp_toml(headless);
    let desired_b64 = B64.encode(&desired);
    format!(
        r#"set -e
f=/home/rmng/.codex/config.toml
install -d -o rmng -g rmng -m700 /home/rmng/.codex
[ -f "$f" ] || : > "$f"
desired="$(mktemp)"
tmp="$(mktemp)"
trap 'rm -f "$desired" "$tmp"' EXIT
base64 -d > "$desired" <<'RMNG_CODEX_MCP'
{desired_b64}
RMNG_CODEX_MCP
# Copy every line EXCEPT the managed [mcp_servers.*] tables. `skip` turns on at an owned table
# header and off at the next header of any kind, so a user's own tables and all top-level keys
# pass through untouched.
awk -v owned='^\[({owned})\]$' -v retired='^[[:space:]]*({retired_keys})[[:space:]]*=' '
  /^[[:space:]]*\[/ {{ skip = ($0 ~ owned); intable = 1 }}
  # Retired bare keys only count BEFORE the first table header — inside a table the same name
  # could legitimately be a user key (e.g. [profiles.x] model = "...").
  !intable && $0 ~ retired {{ next }}
  !skip {{ print }}
' "$f" > "$tmp"
# Collapse the blank lines the removal may have left at EOF, then append the managed tables.
awk 'BEGIN {{ blank = 0 }}
     /^[[:space:]]*$/ {{ blank++; next }}
     {{ while (blank-- > 0) print ""; blank = 0; print }}
' "$tmp" > "$f"
if [ -s "$f" ]; then printf '\n' >> "$f"; fi
cat "$desired" >> "$f"
chown rmng:rmng "$f"
chmod 600 "$f"
"#
    )
}

/// The `rmng-cli` agent skill — single source of truth (hardcoded here). Written into every
/// clone at `~/.claude/skills/rmng-cli/SKILL.md` (Claude Code + the inner Cursor Claude Code)
/// and `~/.agents/skills/rmng-cli/SKILL.md` (Codex), so any agent can load it on
/// demand to learn the `rmng` fleet CLI. Same delivery model as the global prompt / MCP config.
const RMNG_CLI_SKILL_MD: &str = r#"---
name: rmng-cli
description: Use when you need to manage the RMNG clone fleet from inside a clone — list clones, create or destroy clones, open an SSH/exec session into another clone, drive a clone's desktop, or manage clone-source images and agent accounts. Covers the `rmng` command-line tool.
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

- `rmng clone ls` — list clones with live CPU, RAM, status, and each provider's bound account.
  Sub clones are indented under their parent. `--json` gives one object per clone with `stats`
  nested.
- `rmng op ls` — list recent operations (clone / delete / archive / pull / commit / update).
- `rmng op wait <op-id> [--timeout <secs>]` — block until an operation reaches a terminal state.

## Reach another clone

- `rmng clone ssh <clone>` — print a ready-to-paste `ssh` command for a clone.
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
  (above), not `desktop`.

## Create clones

Four create verbs. All of them take `--from <image>` (required; `rmng image ls` lists valid
references) and share the flags in "Common create flags" below.

- `rmng clone create <hostname> --from <image>` — exact hostname (a DNS label), no ticket.
  Takes `--preset <name>` / `--no-preset`.
- `rmng clone create-from-ticket <link-or-id> --from <image>` — clone for an EXISTING Linear ticket. The
  hostname derives from the ticket id (`WE-142` → `<prefix>we-142`) and **the preset is
  auto-selected from the ticket's team prefix** — there is deliberately no `--preset` here.
  Also takes `--agent-instructions` / `--claude-instructions` (appended to the defaults,
  taking precedence).
- `rmng clone create-with-new-ticket --from <image> --team <key> --title <t>` — CREATE a Linear ticket,
  then clone for it. `--team` is a Linear team key like `we`, and it must be a label on some
  preset: that preset is the one used, and its Linear API key opens the issue. Description via
  `--description <markdown>` or `--description-file <path>` (`-` = stdin, which is the sane
  way to pass a multi-line body). Same instruction flags as `create-from-ticket`.
- `rmng clone create-plain --from <image> --title <t>` — no-ticket clone with a title-derived
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

## Images & accounts

- `rmng image ls` — list clone-source images. `rmng image pull [ref]`,
  `rmng image commit <clone> --as <name>`, `rmng image rm <ref>`.
- `rmng account ls [--provider claude|codex]` — list imported accounts + usage windows.
- `rmng account rm <email> [--codex]` — delete an imported account, moving any clones off it.

## Tips

- Prefer `rmng clone exec <clone> -- …` over hand-rolled SSH when you just need to run one
  command elsewhere.
- Everything is addressed by **clone id** (the first column of `rmng clone ls`).
- `rmng clone select <clone>` points the operator's *viewer* at a clone — it does NOT change
  which clone your other commands target.
"#;

/// The `rmng-cli` skill TarEntries: the same SKILL.md at both skill locations.
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
/// `~/.claude/CLAUDE.md` and Codex's `~/.codex/AGENTS.md` — plus the generated Codex config and
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

/// Interactive Claude Code (and the inner Cursor agent / any human `claude`) reads its MCP servers
/// from `~/.claude.json`'s top-level `mcpServers` key. That file is state-bearing (Claude Code
/// accumulates project history in it), so we **jq-merge** the two managed servers rather than
/// overwrite it — matching how the template seeds `linear` (`template/setup/30-user.sh`). `linear`
/// is always set; `desktop` (the clone-daemon computer-use MCP on :9004) is set on headed clones
/// and deleted on headless ones (there is no daemon there). `${LINEAR_API_KEY}` is stored literally
/// — Claude Code expands it from the session env at runtime, so the single quotes below are
/// load-bearing (bash must not expand it). Runs as root via docker exec; re-chowns to rmng.
pub(crate) fn claude_mcp_script(headless: bool) -> String {
    // The jq program is generated from the managed MCP set (single source of truth). It is
    // single-quoted below so bash does not expand the literal `${LINEAR_API_KEY}` inside it —
    // Claude Code expands it from the session env at runtime.
    let program = claude_mcp_jq_program(headless);
    format!(
        r#"set -e
f=/home/rmng/.claude.json
[ -s "$f" ] || printf '{{}}' > "$f"
tmp="$(mktemp)"
jq '{program}' "$f" > "$tmp"
cat "$tmp" > "$f"
rm -f "$tmp"
chown rmng:rmng "$f"
chmod 600 "$f"
"#
    )
}

fn claude_mcp_stamp_path() -> &'static str {
    "etc/rmng/claude-mcp"
}

/// Desired stamp value — changes with the headless bit (and the `v1` shape tag, bumped if the
/// managed server set changes), so the reconciler re-applies `claude_mcp_script` exactly when the
/// desired `~/.claude.json` MCP set would differ.
fn claude_mcp_desired(headless: bool) -> String {
    format!("v1 headless={headless}")
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

pub(crate) fn codex_prepare_script() -> &'static str {
    r#"set -e
install -d -o rmng -g rmng -m700 /home/rmng/.codex
install -d -o rmng -g rmng -m755 /home/rmng/.config /home/rmng/.config/rmng /home/rmng/.claude
install -d -o rmng -g rmng -m755 /home/rmng/.claude/skills/rmng-cli /home/rmng/.agents/skills/rmng-cli
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

/// Prepare a clone's filesystem for the SSH material upload, and run the one-time fleet-key
/// migration. `fleet_body` is the base64 body (field 2) of the shared fleet public key; when
/// non-empty it gates the migration so we only ever delete the *fleet* key, never a user's own.
///
/// Migration: older clones carried the shared fleet key at `~/.ssh/id_ed25519`. GNOME's
/// `gcr-ssh-agent` auto-adopts any `~/.ssh` private key into the login keyring and then crashes
/// on it during `ssh`'s session-bind, wedging ALL ssh/git in the clone (the agent can't sign any
/// key). Now that the key is provisioned outside `~/.ssh` (see [`crate::ssh::CLONE_FLEET_KEY_TAR`]),
/// delete the poisoned copy — only when it exactly matches the fleet key — and kick
/// `gcr-ssh-agent` so it respawns clean (it is socket-activated; a fresh spawn re-scans `~/.ssh`,
/// now without the key). Idempotent: on an already-migrated clone the `id_ed25519` file is gone,
/// so the guarded block is a no-op.
///
/// This also runs on the **create** path (`provision.rs`), not just the reconcile one: a clone
/// created FROM a source image that was committed while the old layout was live inherits the
/// poisoned key baked into that image, and the create path stamps [`SSH_STAMP_VERSION`] itself —
/// which would otherwise make `ensure_ssh_ready` short-circuit forever and leave the clone wedged
/// from birth.
pub(crate) fn ssh_prepare_script(fleet_body: &str) -> String {
    let mut s = String::from(
        "set -e\n\
         install -d -o rmng -g rmng -m700 /home/rmng/.ssh\n\
         mkdir -p /home/rmng/.config/rmng/ssh\n\
         chown rmng:rmng /home/rmng/.config /home/rmng/.config/rmng /home/rmng/.config/rmng/ssh 2>/dev/null || true\n\
         chmod 700 /home/rmng/.config/rmng/ssh\n\
         mkdir -p /etc/ssh\n",
    );
    if !fleet_body.is_empty() {
        s.push_str(&format!(
            "if [ -f /home/rmng/.ssh/id_ed25519.pub ] && \
[ \"$(awk '{{print $2}}' /home/rmng/.ssh/id_ed25519.pub 2>/dev/null)\" = \"{fleet_body}\" ]; then\n\
  rm -f /home/rmng/.ssh/id_ed25519 /home/rmng/.ssh/id_ed25519.pub\n\
  pkill -u 1000 -f /usr/libexec/gcr-ssh-agent 2>/dev/null || true\n\
  echo 'rmng-migrate: removed poisoned ~/.ssh/id_ed25519 (fleet key relocated out of ~/.ssh)'\n\
fi\n"
        ));
    }
    s
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

/// Restart the clone-daemon after a binary refresh — but only if its unit is present. Headless
/// clones DELETE `rmng-clone-daemon.service` (control-server `provision.rs` HEADLESS_DISABLE_SCRIPT),
/// so an unconditional `systemctl --user restart` exits 5 ("unit not loaded") and would abort the
/// whole payload reconcile before the agent-wrapper restart + payload stamp ever run — permanently
/// wedging binary refreshes on headless clones. Guard on `systemctl cat`: present ⇒ restart (a real
/// restart failure still surfaces under `set -e` on headed clones); absent ⇒ skip cleanly.
fn restart_clone_daemon_script() -> &'static str {
    r#"set -e
if runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user cat rmng-clone-daemon.service >/dev/null 2>&1; then
  runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user restart rmng-clone-daemon.service
else
  echo "rmng-clone-daemon.service absent (headless clone) — skipping restart"
fi
"#
}

/// Write the systemd drop-in that clears the retired inference vars from the agent-wrapper's
/// environment, then restart it.
///
/// [`RETIRED_ENV_KEYS`] rewrites `/etc/environment`, which is enough for anything behind a PAM
/// login (an SSH session, a fresh GUI login). It is NOT enough for the agent-wrapper: the same
/// vars were also baked into the CONTAINER's `Config.Env` at create time
/// (`docker::CreateSpec::env`), Docker environment is immutable without recreating the container,
/// and systemd inherits it from PID 1. So the wrapper kept dialling the dead `/cc` router and
/// every chat turn failed with `API Error: 405` — while a shell in the same clone worked fine,
/// which is exactly the sort of split that wastes an afternoon.
///
/// `Environment=KEY=` (empty, no value) is the fix: a unit-level assignment overrides the
/// inherited one, and Claude Code treats an empty `ANTHROPIC_BASE_URL` as unset, falling back to
/// the credentials file the server injects. `UnsetEnvironment=` would be cleaner but is systemd
/// ≥ 248 and applies after `Environment=`, so this form is both older-safe and unambiguous.
fn agent_wrapper_env_dropin_script() -> String {
    let unsets = RETIRED_ENV_KEYS
        .iter()
        .map(|k| format!("Environment={k}=\n"))
        .collect::<String>();
    // The user manager caches `/etc/environment` at ITS startup, via the
    // `environment.d` → `/etc/environment` symlink and systemd's environment-generator. Rewriting
    // the file does not touch that cache, and neither does the drop-in above — which reaches only
    // `agent-wrapper.service`.
    //
    // That cache is not inert: `web::desktop_session_env` harvests
    // `systemctl --user show-environment` and seeds it into every `rmng exec` and every termplane
    // tmux terminal. So without this, the reconciler cleans `/etc/environment` while the exec path
    // keeps re-injecting the dead endpoint into every new shell — forever, since the cache clears
    // only on container restart. Measured on CT 105 before the fix: 33 of 35 clones dirty.
    let unset_args = RETIRED_ENV_KEYS.join(" ");
    format!(
        r#"set -e
d=/home/rmng/.config/systemd/user/agent-wrapper.service.d
install -d -o rmng -g rmng -m755 "$d"
cat > "$d/10-rmng-retired-env.conf" <<'RMNG_DROPIN'
# Managed by RMNG. Clears inference vars baked into the container's Docker Env by an older
# control-server; those cannot be removed by rewriting /etc/environment.
[Service]
{unsets}RMNG_DROPIN
chown rmng:rmng "$d/10-rmng-retired-env.conf"
chmod 644 "$d/10-rmng-retired-env.conf"
runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user daemon-reload
# Drop the retired keys from the user manager's own cached environment block, so
# `show-environment` (and everything seeded from it) stops handing out a dead endpoint.
# Best-effort: a clone whose user manager is not up yet gets it on the next pass.
runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user unset-environment {unset_args} || true
runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user restart agent-wrapper.service
"#
    )
}

fn wrapper_env_stamp_path() -> &'static str {
    "etc/rmng/wrapper-env"
}

/// Stamp value — bumped by editing [`RETIRED_ENV_KEYS`], so the drop-in is rewritten exactly
/// when its content would differ.
///
/// `v2` adds the `systemctl --user unset-environment` step. The version prefix is what forces
/// clones already stamped `v1` to re-run: the key list alone is unchanged, so without the bump
/// every clone that had reconciled once would keep its stale manager environment forever — the
/// exact shape of the bug this step fixes.
fn wrapper_env_desired() -> String {
    format!("v2 {}", RETIRED_ENV_KEYS.join(","))
}

/// Apply the drop-in once per clone, stamped.
///
/// Deliberately NOT gated on `/etc/environment` having changed. That gate is right for the
/// restart-to-pick-up-new-values case, but the baked container env is a SEPARATE problem: on a
/// clone whose `/etc/environment` is already correct — every clone that has reconciled once —
/// the gate never fires and the drop-in would never be written. That is exactly what happened on
/// the first CT 106 migration.
async fn ensure_wrapper_env_dropin(app: &App, clone_id: &str) -> Result<bool> {
    let desired = wrapper_env_desired();
    if read_stamp(app, clone_id, wrapper_env_stamp_path(), "wrapper env")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }
    exec_ok(
        app,
        clone_id,
        &agent_wrapper_env_dropin_script(),
        "clear retired vars from agent-wrapper",
    )
    .await?;
    app.docker
        .upload_tar(
            clone_id,
            vec![TarEntry {
                path: wrapper_env_stamp_path().to_string(),
                data: format!("{desired}\n").into_bytes(),
                mode: 0o644,
                uid: 0,
                gid: 0,
            }],
        )
        .await
        .with_context(|| format!("{clone_id}: writing wrapper env stamp"))?;
    Ok(true)
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

fn tmp_mount_mask_script() -> &'static str {
    r#"set -e
systemctl mask tmp.mount >/dev/null 2>&1 || {
  mkdir -p /etc/systemd/system
  ln -sf /dev/null /etc/systemd/system/tmp.mount
}
systemctl daemon-reload >/dev/null 2>&1 || true
"#
}

/// Install the polkit rule that authorizes the `sudo` group — the backport of the phase-10
/// template step (`template/setup/10-desktop.sh`) onto clones built from an older image.
///
/// A clone has no display manager, so its only logind session is the one linger opens:
/// `Class=manager`, no seat, no TTY. polkit cannot resolve an auth cookie to a session of that
/// class, so every `auth_admin*` action fails at the agent handshake with
/// `GDBus.Error:org.freedesktop.PolicyKit1.Error.Failed: No session for cookie` regardless of
/// which agent answers — installing a graphical agent does not help. Returning `YES` authorizes
/// without an authentication step, so there is no agent to find and no cookie to resolve.
///
/// Scoped to `sudo`, which the template already grants `NOPASSWD:ALL` in `/etc/sudoers.d`, so
/// this confers no privilege the clone user did not already have.
///
/// No `systemctl restart polkit`: polkitd watches `rules.d` with inotify and picks up a new file
/// within a second or two (verified), and restarting it would tear down in-flight authorizations
/// on every pass. Deliberately NOT `install -d` for the directory — polkitd ships it `0750
/// root:polkitd` and `install -d` re-modes an existing directory, which would publish the rules
/// to every user; `mkdir -p` leaves the packaged mode intact.
///
/// Idempotent by content compare, so a clone that already has the rule is a no-op and produces
/// no log line.
fn polkit_sudo_rule_script() -> &'static str {
    r#"set -e
dest=/etc/polkit-1/rules.d/49-rmng-sudo-nopasswd.rules
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
cat > "$tmp" <<'RULES'
// A clone has no display manager, so its only logind session is Class=manager (linger's
// bare `systemd --user`) with no seat or TTY. polkit cannot map an auth cookie back to such
// a session, so any auth_admin* action fails with "No session for cookie" whatever agent is
// running. Return YES to skip authentication entirely rather than fix an unfixable lookup.
// Limited to `sudo`, which phase 30 already grants NOPASSWD:ALL via /etc/sudoers.d.
polkit.addRule(function (action, subject) {
    if (subject.isInGroup("sudo")) {
        return polkit.Result.YES;
    }
});
RULES
if [ -f "$dest" ] && cmp -s "$tmp" "$dest"; then
  exit 0
fi
mkdir -p /etc/polkit-1/rules.d
install -m 0644 -o root -g root "$tmp" "$dest"
echo "installed polkit sudo-group rule at $dest"
"#
}

fn etc_environment_sync_script(desired_env: &str) -> String {
    let desired_b64 = B64.encode(desired_env);
    // One `KEY` per line, appended to the strip-list below. These are stripped but never
    // re-appended, which is what actually removes a key we no longer emit — see RETIRED_ENV_KEYS.
    let retired = RETIRED_ENV_KEYS.join("\n");
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
{{ grep -E '^[A-Za-z_][A-Za-z0-9_]*=' "$desired" | sed 's/=.*//'; cat <<'RMNG_RETIRED_KEYS'
{retired}
RMNG_RETIRED_KEYS
}} | sed '/^$/d' | sort -u > "$keys_file"
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

fn preset_for_clone<'a>(cfg: &'a wire::AppConfig, host: &wire::RmngClone) -> Option<&'a wire::Preset> {
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
        if let Some(preset) = crate::linear::pick_preset_by_prefix(&cfg.presets, prefix) {
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
            if stream == "stdout" && line.contains(marker) {
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
        == Some(SSH_STAMP_VERSION)
    {
        return Ok(());
    }
    // Base64 body of the fleet pubkey — gates the ~/.ssh/id_ed25519 migration to the fleet key only.
    let fleet_body = crate::ssh::fleet_public_key(&app.config().data_dir)
        .ok()
        .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_default();
    exec_ok(app, clone_id, &ssh_prepare_script(&fleet_body), "prepare ssh dirs").await?;
    // After `ssh_prepare_script` (which creates ~/.ssh) so the read sees the real state.
    let existing_ssh_config = crate::ssh::read_clone_ssh_config(&app.docker, clone_id).await;
    let entries = crate::ssh::clone_ssh_tar_entries(
        &app.config().data_dir,
        clone_id,
        &app.config().ssh.authorized_keys,
        &existing_ssh_config,
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

async fn ensure_codex_parity(
    app: &App,
    clone_id: &str,
    headless: bool,
    global_prompt: &str,
) -> Result<bool> {
    let entries = codex_parity_entries(headless, global_prompt);
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

/// Keep interactive Claude Code's `~/.claude.json` MCP set in sync (desktop headed-only, linear
/// always). jq-merge via [`claude_mcp_script`], stamped on the headless bit so it only execs when
/// the desired set changes — retrofitting `desktop` onto existing headed clones and removing it
/// from existing headless ones on the reconciler's next pass.
async fn ensure_claude_mcp(app: &App, clone_id: &str, headless: bool) -> Result<bool> {
    let desired = claude_mcp_desired(headless);
    if read_stamp(app, clone_id, claude_mcp_stamp_path(), "claude mcp")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }
    exec_ok(
        app,
        clone_id,
        &claude_mcp_script(headless),
        "sync ~/.claude.json MCP",
    )
    .await?;
    app.docker
        .upload_tar(clone_id, vec![claude_mcp_stamp_entry_for(headless)])
        .await
        .with_context(|| format!("{clone_id}: writing claude mcp stamp"))?;
    Ok(true)
}

fn codex_mcp_stamp_path() -> &'static str {
    "etc/rmng/codex-mcp"
}

/// Desired stamp value — changes with the headless bit (and the `v1` shape tag, bumped if the
/// managed server set changes), so the merge re-runs exactly when the desired tables differ.
fn codex_mcp_desired(headless: bool) -> String {
    format!("v1 headless={headless}")
}

fn codex_mcp_stamp_entry_for(headless: bool) -> TarEntry {
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
async fn ensure_codex_mcp(app: &App, clone_id: &str, headless: bool) -> Result<bool> {
    let desired = codex_mcp_desired(headless);
    if read_stamp(app, clone_id, codex_mcp_stamp_path(), "codex mcp")
        .await?
        .as_deref()
        == Some(desired.as_str())
    {
        return Ok(false);
    }
    exec_ok(
        app,
        clone_id,
        &codex_mcp_merge_script(headless),
        "merge ~/.codex/config.toml MCP",
    )
    .await?;
    app.docker
        .upload_tar(clone_id, vec![codex_mcp_stamp_entry_for(headless)])
        .await
        .with_context(|| format!("{clone_id}: writing codex mcp stamp"))?;
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

async fn reconcile_once(app: &App, warned: &mut HashSet<String>) {
    let hosts: Vec<_> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived && is_safe_id(&h.id))
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
        // Per-clone identity key (`RMNG_PROXY_KEY`): recomputed into `/etc/environment` on every
        // resync so an existing clone picks it up without a recreate. Minted + persisted
        // server-side; never serialized onto `RmngClone`/state. See `crate::clonekey`.
        desired_env.extend(crate::provision::clone_key_env_vars(app, id));
        if let Some(preset) = preset_for_clone(&cfg, h) {
            desired_env.extend(crate::provision::preset_env_vars(preset));
        } else if h.preset_name.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            tracing::warn!(
                target: "clone_reconcile",
                "clone {id}: preset {:?} no longer exists; preserving unmanaged /etc/environment keys",
                h.preset_name
            );
        }
        // Claude Code's default model (ANTHROPIC_MODEL). The create path seeds this same var
        // from the same helper, so a fresh clone already has it and this pass is a no-op
        // content-compare rather than a 30 s-late rewrite.
        desired_env.push(claude_model_env_var());
        let desired_env = crate::provision::clone_etc_environment_conf(&desired_env);
        // Clear the retired inference vars from the agent-wrapper's unit environment. Stamped
        // and independent of the env-change gate below: the vars this removes come from the
        // container's baked Docker Env, not from /etc/environment, so a clone whose file is
        // already correct still needs it.
        match ensure_wrapper_env_dropin(app, id).await {
            Ok(true) => {
                warned.remove(&format!("{id}:wrapper-env"));
                tracing::info!(
                    target: "clone_reconcile",
                    "clone {id}: cleared retired inference vars from the agent-wrapper unit"
                );
            }
            Ok(false) => {
                warned.remove(&format!("{id}:wrapper-env"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:wrapper-env")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: agent-wrapper env drop-in failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: agent-wrapper env drop-in still failing: {e:#}");
                }
            }
        }

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

        match exec_ok(app, id, polkit_sudo_rule_script(), "polkit sudo rule").await {
            Ok(()) => {
                warned.remove(&format!("{id}:polkit"));
            }
            Err(e) => {
                if warned.insert(format!("{id}:polkit")) {
                    tracing::warn!(target: "clone_reconcile", "clone {id}: polkit rule reconcile failed: {e:#}");
                } else {
                    tracing::debug!(target: "clone_reconcile", "clone {id}: polkit rule reconcile still failing: {e:#}");
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
                continue;
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
        assert_eq!(entry.data, b"v2\n");
        assert_eq!(entry.mode, 0o644);
        assert_eq!((entry.uid, entry.gid), (0, 0));
    }

    #[test]
    fn ssh_prepare_script_creates_fleet_dir_and_migrates_poisoned_key() {
        let s = ssh_prepare_script("AAAAC3NzaC1lZDI1NTE5AAAAFLEETBODY");
        // Creates the out-of-~/.ssh fleet key dir.
        assert!(s.contains("mkdir -p /home/rmng/.config/rmng/ssh"));
        // Migration: guarded on the exact fleet pubkey body, removes the poisoned key + kicks gcr.
        assert!(s.contains("AAAAC3NzaC1lZDI1NTE5AAAAFLEETBODY"));
        assert!(s.contains("rm -f /home/rmng/.ssh/id_ed25519 /home/rmng/.ssh/id_ed25519.pub"));
        assert!(s.contains("pkill -u 1000 -f /usr/libexec/gcr-ssh-agent"));
    }

    #[test]
    fn ssh_prepare_script_without_fleet_body_skips_migration() {
        // No fleet key resolved ⇒ never touch ~/.ssh/id_ed25519 (can't verify it's the fleet key).
        let s = ssh_prepare_script("");
        assert!(s.contains("mkdir -p /home/rmng/.config/rmng/ssh"));
        assert!(!s.contains("rm -f /home/rmng/.ssh/id_ed25519"));
    }

    /// The agent-wrapper must have the retired inference vars cleared at the UNIT level.
    ///
    /// Rewriting `/etc/environment` is not enough for it: the same vars were baked into the
    /// container's Docker `Config.Env` at create time, that is immutable without recreating the
    /// container, and systemd inherits it from PID 1. On the first real migration this left the
    /// wrapper dialling the dead `/cc` router — every chat turn `API Error: 405` — while an SSH
    /// shell into the same clone worked, because PAM sessions read the (already fixed) file.
    #[test]
    fn wrapper_restart_clears_the_baked_inference_vars() {
        let script = agent_wrapper_env_dropin_script();
        // A drop-in, not an edit of the template-owned unit — so this reaches existing clones
        // without a template rebuild and cannot be clobbered by one.
        assert!(script.contains("agent-wrapper.service.d"), "{script}");
        assert!(script.contains("[Service]"));
        // Every retired key is assigned empty at the unit level, which overrides the inherited
        // container env. Claude Code treats an empty base URL as unset and falls back to the
        // credentials file the server injects.
        for key in RETIRED_ENV_KEYS {
            assert!(
                script.contains(&format!("Environment={key}=\n")),
                "retired key {key} not cleared for the wrapper:\n{script}"
            );
        }
        // `RMNG_PROXY_KEY` is the clone's identity and must NOT be cleared — the baked copy is
        // stale, but the unit inherits the current one from /etc/environment via the session.
        assert!(!script.contains("Environment=RMNG_PROXY_KEY="), "identity key was cleared");
        // daemon-reload before restart, or the drop-in would not take effect until next boot.
        let reload = script.find("daemon-reload").expect("no daemon-reload");
        let restart = script.find("restart agent-wrapper").expect("no restart");
        assert!(reload < restart, "daemon-reload must precede the restart");

        // The drop-in fixes only agent-wrapper.service. The user MANAGER caches
        // /etc/environment at its own startup, and `web::desktop_session_env` harvests that cache
        // into every `rmng exec` and tmux terminal — so without an explicit unset, the reconciler
        // cleans the file while the exec path keeps handing out the dead endpoint, forever.
        // Measured on CT 105: 33 of 35 clones carried it.
        let unset = script.find("unset-environment").expect("manager env is never cleared");
        for key in RETIRED_ENV_KEYS {
            assert!(
                script[unset..].contains(key),
                "retired key {key} not unset from the user manager env:\n{script}"
            );
        }
        assert!(
            !script[unset..].contains("RMNG_PROXY_KEY"),
            "the identity key must survive the manager-env unset"
        );
    }

    /// The stamp gates the drop-in per clone, so its VERSION is what makes an already-stamped
    /// clone re-run a changed script. Editing the script without bumping the version silently
    /// skips every clone that has reconciled before — which is precisely the fleet you are
    /// trying to fix.
    #[test]
    fn wrapper_env_stamp_version_tracks_the_script() {
        let desired = wrapper_env_desired();
        assert!(
            desired.starts_with("v2 "),
            "bump this test with the stamp; v1 predates the manager-env unset: {desired}"
        );
        // The key list rides along, so adding a retired key also re-stamps.
        for key in RETIRED_ENV_KEYS {
            assert!(desired.contains(key), "stamp does not cover {key}: {desired}");
        }
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
    /// be rewritten wholesale every reconcile pass, silently reverting any hand-edit within
    /// ~30 s. The failure modes are all in shell, not Rust, so this runs the REAL generated
    /// script against a real file rather than asserting on its text.
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
            let script = codex_mcp_merge_script(headless)
                .replace("/home/rmng/.codex", codex.to_str().unwrap())
                .replace("chown rmng:rmng", "true")
                .replace("install -d -o rmng -g rmng -m700", "mkdir -p");
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("run codex merge script");
            assert!(out.status.success(), "script failed: {}", String::from_utf8_lossy(&out.stderr));
            std::fs::read_to_string(&cfg).unwrap()
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

        // A headed→headless flip REMOVES `desktop` (no daemon there) and keeps everything else.
        let hl = run(true);
        assert!(!hl.contains("[mcp_servers.desktop]"), "headless kept a dead endpoint:\n{hl}");
        assert!(!hl.contains("127.0.0.1:9004"));
        assert!(hl.contains("[mcp_servers.linear]"));
        assert!(hl.contains("[mcp_servers.my_own]"), "headless dropped the user's own server");
        assert!(hl.contains("model_reasoning_effort = \"high\""));

        // The group-proxy era's dead wiring must be REMOVED, not merely left alone. A merge that
        // only replaces what it currently emits never removes what it used to — the same trap
        // RETIRED_ENV_KEYS exists for. `model_provider = "rmng"` beats the `~/.codex/auth.json`
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
        assert!(!cleaned.contains("model_providers.rmng"), "dead provider table survived:\n{cleaned}");
        assert!(!cleaned.contains("rmng-control:9000"), "dead base_url survived:\n{cleaned}");
        assert!(!cleaned.contains("RMNG_PROXY_KEY"), "dead env_key survived:\n{cleaned}");
        assert!(
            !cleaned.contains("model_provider = "),
            "the bare model_provider key still overrides auth.json:\n{cleaned}"
        );
        // ...but a plain preference RMNG never owned is NOT ours to delete.
        assert!(cleaned.contains("model_reasoning_effort = \"high\""), "{cleaned}");
        // ...and a `model` key INSIDE a user's own table is theirs, not the retired top-level one.
        assert!(cleaned.contains("[profiles.fast]"), "{cleaned}");
        assert!(cleaned.contains("model = \"gpt-5.5\""), "a user's in-table model was stripped:\n{cleaned}");

        // A clone with no config.toml at all gets a valid one rather than an error.
        std::fs::remove_file(&cfg).unwrap();
        let fresh = run(false);
        assert!(fresh.contains("[mcp_servers.linear]"));
        assert!(fresh.contains("[mcp_servers.desktop]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_reconcile_script_removes_provider_credentials() {
        let scripts: Vec<(&str, String)> = vec![
            ("codex_prepare", codex_prepare_script().to_string()),
            ("ssh_bootstrap", ssh_bootstrap_script().to_string()),
            ("polkit_sudo_rule", polkit_sudo_rule_script().to_string()),
            ("tmp_mount_mask", tmp_mount_mask_script().to_string()),
            ("claude_mcp", claude_mcp_script(false)),
            ("claude_mcp_headless", claude_mcp_script(true)),
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
            let e = entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("missing {path}"));
            assert_eq!(e.mode, 0o644);
            assert_eq!((e.uid, e.gid), (1000, 1000));
            assert_eq!(String::from_utf8(e.data.clone()).unwrap(), prompt);
        }
        // The node-agent MCP descriptor is part of the bundle.
        let desc = entries
            .iter()
            .find(|e| e.path == "home/rmng/.config/rmng/mcp.json")
            .expect("missing mcp.json descriptor");
        assert!(String::from_utf8(desc.data.clone()).unwrap().contains("\"linear\""));
        let agents = entries
            .iter()
            .find(|e| e.path == "home/rmng/.codex/AGENTS.md")
            .expect("missing Codex AGENTS.md");
        let agents_body = String::from_utf8(agents.data.clone()).unwrap();
        assert!(agents_body.contains("SENTINEL-A+C"));

        // `~/.codex/config.toml` is deliberately NOT in this set: it is merged in place by
        // `codex_mcp_merge_script` so the operator's own settings survive, not shipped as a tar
        // entry that would overwrite the file. Shipping it here again would silently reintroduce
        // the clobber.
        assert!(
            !entries.iter().any(|e| e.path == "home/rmng/.codex/config.toml"),
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
    fn claude_mcp_script_sets_desktop_headed_and_deletes_it_headless() {
        let headed = claude_mcp_script(false);
        assert!(headed.contains(".mcpServers.linear ="));
        assert!(headed.contains(".mcpServers.desktop ="));
        assert!(headed.contains("http://127.0.0.1:9004"));
        assert!(!headed.contains("del(.mcpServers.desktop)"));

        let headless = claude_mcp_script(true);
        assert!(headless.contains(".mcpServers.linear ="));
        assert!(headless.contains("del(.mcpServers.desktop)"));
        assert!(!headless.contains(".mcpServers.desktop ="));

        // ${LINEAR_API_KEY} must be stored literally so Claude Code expands it from the session
        // env at runtime. The whole jq program is single-quoted in the bash caller (`jq '…'`), so
        // bash does not expand the literal env reference embedded in the linear header.
        assert!(headed.contains(r#""Bearer ${LINEAR_API_KEY}""#));
        assert!(headless.contains(r#""Bearer ${LINEAR_API_KEY}""#));
        assert!(headed.contains("jq '") && headed.contains("' \"$f\""));

        // The stamp value tracks the headless bit so the reconciler re-applies on a state change.
        assert_ne!(claude_mcp_desired(false), claude_mcp_desired(true));
    }

    #[test]
    fn agent_configs_omit_desktop_mcp_when_headless() {
        // Headless clones have no desktop (the clone-daemon on :9004 is deleted), so the shared
        // `desktop` MCP must disappear from every generated agent config while `linear` stays.
        let codex = codex_mcp_toml(true);
        assert!(!codex.contains("[mcp_servers.desktop]"));
        assert!(!codex.contains("127.0.0.1:9004"));
        assert!(codex.contains("[mcp_servers.linear]"));

        // Headed keeps desktop.
        let codex_headed = codex_mcp_toml(false);
        assert!(codex_headed.contains("[mcp_servers.desktop]"));
        assert!(codex_headed.contains("127.0.0.1:9004"));

        // The node-agent descriptor and the Claude jq program agree with it.
        assert!(claude_mcp_jq_program(true).contains("del(.mcpServers.desktop)"));
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
            let e = entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("missing {path}"));
            assert_eq!(e.mode, 0o644);
            assert_eq!((e.uid, e.gid), (1000, 1000));
            let body = String::from_utf8(e.data.clone()).unwrap();
            assert!(body.starts_with("---\nname: rmng-cli\n"), "SKILL.md needs skill frontmatter");
            assert!(body.contains("rmng clone ls") && body.contains("rmng clone exec"));
        }
        // The prepare script creates both skill directories.
        let prep = codex_prepare_script();
        assert!(prep.contains("/home/rmng/.claude/skills/rmng-cli"));
        assert!(prep.contains("/home/rmng/.agents/skills/rmng-cli"));
    }

    #[test]
    fn managed_mcp_is_the_single_source_for_all_emitters() {
        // Headed: every emitter renders both managed servers with the right auth form.
        let codex = codex_mcp_toml(false);
        assert!(codex.contains("[mcp_servers.desktop]") && codex.contains("http://127.0.0.1:9004"));
        assert!(codex.contains("[mcp_servers.linear]"));
        assert!(codex.contains("bearer_token_env_var = \"LINEAR_API_KEY\""));

        let jq = claude_mcp_jq_program(false);
        assert!(jq.contains(r#".mcpServers.desktop = {"type":"http","url":"http://127.0.0.1:9004"}"#));
        assert!(jq.contains(r#""Authorization":"Bearer ${LINEAR_API_KEY}""#));

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
        assert!(claude_mcp_jq_program(true).contains("del(.mcpServers.desktop)"));
        let desc_hl: serde_json::Value = serde_json::from_str(&mcp_descriptor_json(true)).unwrap();
        assert_eq!(desc_hl.as_array().unwrap().len(), 1);
        assert_eq!(desc_hl[0]["name"], "linear");
    }


    #[test]
    fn codex_parity_stamp_hash_changes_when_config_changes() {
        let original =
            codex_parity_stamp_entry_for(&codex_parity_entries(false, "guide"));
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
    fn polkit_sudo_rule_script_is_idempotent_and_preserves_rules_dir_mode() {
        let script = polkit_sudo_rule_script();
        assert!(script.contains("/etc/polkit-1/rules.d/49-rmng-sudo-nopasswd.rules"));
        assert!(script.contains(r#"subject.isInGroup("sudo")"#));
        assert!(script.contains("polkit.Result.YES"));
        // Content compare before write: an already-reconciled clone must be a silent no-op.
        assert!(script.contains(r#"cmp -s "$tmp" "$dest""#));
        assert!(script.contains("install -m 0644 -o root -g root"));
        // `install -d` would re-mode polkitd's 0750 root:polkitd rules.d to world-readable.
        assert!(!script.contains("install -d"));
        assert!(script.contains("mkdir -p /etc/polkit-1/rules.d"));
        // polkitd hot-reloads rules.d via inotify; a restart would drop in-flight auths.
        assert!(!script.contains("systemctl restart polkit"));
        // AUTH_ADMIN* would reintroduce the agent handshake this rule exists to avoid.
        assert!(!script.contains("AUTH_ADMIN"));
    }

    /// The rule body must match `template/setup/10-desktop.sh` byte for byte. The reconciler
    /// decides whether to act by `cmp`-ing against the on-disk file, so a one-character drift
    /// (even in a comment) makes it rewrite the rule on every 30s pass of every new-image
    /// clone forever. Caught exactly that way: the two copies disagreed on one comment line.
    #[test]
    fn polkit_rule_body_matches_the_template_phase_10_copy() {
        let template = include_str!("../../../template/setup/10-desktop.sh");
        // Compare the whole heredoc body, comments included — `cmp` does not care that a
        // differing line is only a comment, so neither can this test.
        let extract = |s: &str| {
            let open = "<<'RULES'\n";
            let start = s.find(open).expect("RULES heredoc present") + open.len();
            let end = s[start..].find("\nRULES\n").expect("heredoc terminated") + start + 1;
            s[start..end].to_string()
        };
        assert_eq!(
            extract(polkit_sudo_rule_script()),
            extract(template),
            "reconciler and template phase 10 must write identical polkit rules"
        );
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
            .replace("rmdir /home/rmng/.config/environment.d", "rmdir /nonexistent");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run env sync script");
        assert!(out.status.success(), "script failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).contains(ENV_CHANGED_MARKER)
    }

    /// A key RMNG stops emitting is NOT removed by the desired-keys strip-list — it would
    /// survive on the clone forever. `ANTHROPIC_BASE_URL` pointing at the deleted
    /// `rmng-cliproxy` container would then fail every agent request with no self-heal, so
    /// [`RETIRED_ENV_KEYS`] must strip it while leaving operator-owned lines untouched.
    #[test]
    fn env_sync_strips_retired_keys_but_keeps_operator_lines() {
        let dir = std::env::temp_dir().join(format!("rmng-envretire-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let etc = dir.join("environment");
        // A clone as the group-proxy era left it, plus the operator's own customizations.
        std::fs::write(
            &etc,
            "# operator's own notes\n\
             ANTHROPIC_BASE_URL=http://rmng-cliproxy:9010/cc\n\
             CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1\n\
             ANTHROPIC_AUTH_TOKEN=deadbeef\n\
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
        assert!(changed, "stripping dead keys is a change; the agent-wrapper must restart");
        let body = std::fs::read_to_string(&etc).unwrap();

        // The dead group-proxy wiring is gone.
        assert!(!body.contains("ANTHROPIC_BASE_URL"), "stale proxy URL survived:\n{body}");
        assert!(!body.contains("ANTHROPIC_AUTH_TOKEN"), "stale router token survived:\n{body}");
        assert!(!body.contains("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"), "{body}");
        // `RMNG_PROXY_KEY` outlived the proxy as the clone's identity token — it must NOT be
        // treated as retired (sub-clone parent detection + clone↔clone SSH both read it).
        assert!(body.contains("RMNG_PROXY_KEY=keepme"), "identity key was dropped:\n{body}");
        assert!(body.contains("RMNG_CONTROL_URL=http://rmng-control:9000"), "{body}");
        // Operator-owned content is never touched.
        assert!(body.contains("# operator's own notes"), "comment lost:\n{body}");
        assert!(body.contains("MY_OWN_VAR=hello"), "operator var lost:\n{body}");
        assert!(body.contains("export EDITOR=vim"), "export line lost:\n{body}");

        // Idempotent: a second pass with the same desired env is not a change.
        let changed =
            run_env_sync(&dir, &etc, "RMNG_CONTROL_URL=http://rmng-control:9000\nRMNG_PROXY_KEY=keepme\n");
        assert!(!changed, "a converged clone must not restart its agent-wrapper every pass");

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
            std::fs::read_to_string(&etc).unwrap().contains("rmng-control:9000"),
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
