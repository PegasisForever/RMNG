//! `AppConfig` — every setting, edited via the Settings UI (no hand-edited files).
//!
//! The preset Linear keys live in the server's `config.json` (0600) and **are** handed to
//! the browser. That is deliberate. The browser lists Linear issues itself, so it needs a
//! key of its own, and this server answers only on a Tailscale-only network. The server
//! still holds the same keys for its own calls, and injects each one into its preset's
//! clones. `GET /api/config` returns [`AppConfigRedacted`], which carries
//! each key verbatim; `PUT /api/config` still takes them as write-only fields, where a
//! blank value means "keep the stored one". Keys stay out of `ControlState`, which is a
//! separate document with its own file and its own broadcast. The Docker backend has no
//! secret (local unix socket), so [`DockerConfig`] passes through the redacted view intact.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::control::{LayoutPreset, MonitorSpec};

/// Hardcoded control-server ports and paths (formerly the Settings "Advanced" pane).
/// Nothing here is user-serviceable: changing a port would desync the clones that bake
/// these values in at provision, and the directories are baked into volume mounts — so
/// they live in code, not in `config.json` (old files still carrying the keys load fine;
/// serde drops unknown fields).
pub const PORT_WEB: u16 = 9000;
pub const PORT_VIDEO: u16 = 9001;
/// The clone-daemon's in-clone HTTP MCP port. The control-server proxies desktop/window
/// tools (`POST /api/hosts/:id/mcp`) to `http://{clone}:{PORT_DAEMON_MCP}`; each
/// clone-daemon listens here (set via `RMNG_DAEMON_MCP_PORT`). Same value for every clone.
pub const PORT_DAEMON_MCP: u16 = 9004;
/// The control-server's port-forward data plane. The viewer opens one TCP connection here
/// per accepted local socket; the server splices to the clone.
pub const PORT_FORWARD: u16 = 9005;
/// The bastion `sshd` port (jump host into clones).
pub const PORT_BASTION: u16 = 2222;
/// agent-wrapper port on each clone (chat proxy + reload nudge).
pub const AGENT_PORT: u16 = 4096;
/// Data directory (state.json, chats, uploads, hosts mounts, secrets). Fixed at `/data`
/// in the container (the mounted volume).
pub const DATA_DIR: &str = "data";
/// Unix socket the clone-daemons connect to (media plane over `SCM_RIGHTS`, not the
/// network). Fixed by the container's shared sock volume.
pub const CLONE_SOCKET: &str = "/srv/rmng-sock/clones.sock";
/// Docker daemon unix socket the control-server drives clones through.
pub const DOCKER_SOCKET: &str = "/var/run/docker.sock";
/// CIDR for the user-defined `rmng` bridge network (`.1` gateway, `.2` control-server,
/// `.10+` clone pool). Baked into the network + every clone's static IP at first-run setup.
pub const DOCKER_SUBNET: &str = "10.99.0.0/24";
/// Registry reference the in-product self-update pulls the control-server image from
/// (and digest-compares against for update-available detection).
pub const SERVER_IMAGE: &str = "pegasis0/rmng:latest";
/// The shared Docker build infra (pull-through Hub mirror + remote BuildKit) always runs.
pub const BUILD_INFRA_ENABLED: bool = true;
/// Images + cache size for that build infra.
pub const REGISTRY_IMAGE: &str = "registry:2.8.3";
pub const BUILDKIT_IMAGE: &str = "moby/buildkit:v0.17.2";
pub const BUILDKIT_CACHE_GB: u32 = 40;
/// Usage poll intervals (seconds, floored at 15 by the pollers). Nobody changes these.
pub const CLAUDE_POLL_SECS: u64 = 600;
pub const CODEX_POLL_SECS: u64 = 600;
/// The Codex poller always fetches usage (the `usagePolling=false` escape hatch for a
/// drifting `/wham/usage` shape is gone; a drift is fixed in code now).
pub const CODEX_USAGE_POLLING: bool = true;

/// Chroma subsampling mode for the port-1 viewer video stream.
///
/// `Yuv420` is today's hardware path (one `W×H` NV12 H.264 stream per monitor).
/// `Yuv444` recovers full chroma using the RDP **AVC444** packing carried in a single
/// double-height `W×2H` stream (main view stacked over an auxiliary chroma view),
/// reassembled to 4:4:4 on the GPU at the viewer. Server-wide, chosen at launch
/// (`config.chroma`); the viewer learns the active mode from the port-1 connect handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum ChromaMode {
    /// 4:2:0 — today's single-stream hardware path (default).
    #[default]
    Yuv420,
    /// 4:4:4 — AVC444 double-height stream (≤1440p per monitor).
    Yuv444,
}

/// SSH access settings. The control-server always runs a jump-only bastion `sshd`
/// (no enable/disable toggle — same as the SMB share); these keys are installed on the
/// bastion AND every clone. An empty `authorized_keys` means no keys get pasted in.
/// Public keys are NOT secret — the whole struct passes through [`AppConfigRedacted`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct SshConfig {
    /// Authorized SSH public keys, one full line each (`ssh-ed25519 AAAA… comment`).
    #[serde(default)]
    pub authorized_keys: Vec<String>,
}

/// One environment variable in a preset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct EnvVar {
    pub key: String,
    #[serde(default)]
    pub value: String,
}

/// A clone preset: a Linear identity (API key + the ticket-id prefixes that auto-select
/// this preset when cloning from a ticket) plus a named set of environment variables,
/// applied to a clone's session at creation (written to `/etc/environment`; the Linear key is additionally
/// injected as `LINEAR_API_KEY`, which auths the clone's `linear` MCP). Vars that must
/// ALWAYS be present (e.g. `XDG_CURRENT_DESKTOP`) are NOT presets — they're baked into the
/// template's base session env by `template/setup/30-user.sh` at template build, inherited by
/// every clone.
/// NOT TS-exported: the browser reads [`PresetRedacted`], which carries the same
/// `linear_key` verbatim and differs only in what a `PUT` may write back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub name: String,
    /// Ticket-id prefixes (Linear team keys, e.g. `DEV`) that auto-select this preset,
    /// matched case-insensitively against the ticket's prefix (`DEV-196` → `dev`); first
    /// matching preset in config order wins. Named `labels` for config back-compat.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Linear personal API key. A credential, but not one this server keeps to itself: it is
    /// injected into clones as `LINEAR_API_KEY` and handed to the browser through
    /// [`PresetRedacted`], because the browser lists its own issues.
    #[serde(default)]
    pub linear_key: String,
    /// Default Claude account for clones of this preset — an account *selection* in the usual
    /// form: an email, `auto`, `none`, or `group:<pool>`. Deliberately the same string every
    /// other account field takes, so a preset can pin a specific account, point at a pool, or
    /// opt out of a token entirely, with no preset-only concept to learn.
    ///
    /// Empty = "no opinion", which is NOT the same as `auto`: it lets the resolution chain fall
    /// through to the next step (see `web::effective_accounts_preset`), whereas an explicit
    /// `auto` is a real choice that stops the chain.
    #[serde(default)]
    pub claude_account: String,
    /// Default Codex account, same forms. Independent of `claude_account`.
    #[serde(default)]
    pub codex_account: String,
    /// Optional per-preset text appended (after `"\n\n"`) to the global agent playbook for
    /// clones of this preset. Empty ⇒ no append. Non-secret. (Layer **d**: node-agent extra,
    /// this preset only.)
    #[serde(default)]
    pub agent_playbook: String,
    /// Optional per-preset text appended (after `"\n\n"`) to the global agent prompt for clones
    /// of this preset — written to EVERY agent's global rules file (CLAUDE.md / AGENTS.md).
    /// Empty ⇒ no append. Non-secret. (Layer **c**: global prompt, all agents, this preset only.)
    #[serde(default)]
    pub global_prompt: String,
    /// Optional per-preset startup script, edited in Settings. Runs as the clone user as
    /// the last step of create/fork when the caller opts in (frontend defaults on, CLI
    /// defaults off). Empty ⇒ nothing to run. Non-secret.
    #[serde(default)]
    pub startup_script: String,
    /// The FULL Dockerfile this preset's clones build from, edited in Settings by
    /// anyone (single user, trusted network, no auth). Defaults to
    /// `FROM pegasis0/rmng-template:latest`. May hold secrets (ENV lines) — accepted:
    /// baked layers are readable by anyone with daemon access. The Linear key stays
    /// OUT: it remains a preset field, injected at runtime as `LINEAR_API_KEY`.
    #[serde(default = "default_preset_dockerfile")]
    pub dockerfile: String,
}

impl Default for Preset {
    // A preset without Dockerfile text builds the base template: the default is the
    // base Dockerfile, not an empty string (an empty Dockerfile cannot build).
    fn default() -> Self {
        Self {
            name: String::new(),
            labels: Vec::new(),
            linear_key: String::new(),
            claude_account: String::new(),
            codex_account: String::new(),
            agent_playbook: String::new(),
            global_prompt: String::new(),
            startup_script: String::new(),
            dockerfile: default_preset_dockerfile(),
        }
    }
}

impl Preset {
    pub fn redacted(&self) -> PresetRedacted {
        PresetRedacted {
            name: self.name.clone(),
            labels: self.labels.clone(),
            linear_key: self.linear_key.clone(),
            claude_account: self.claude_account.clone(),
            codex_account: self.codex_account.clone(),
            agent_playbook: self.agent_playbook.clone(),
            global_prompt: self.global_prompt.clone(),
            startup_script: self.startup_script.clone(),
            dockerfile: self.dockerfile.clone(),
        }
    }
}

/// A preset as shown to the browser: every field of [`Preset`], Linear key included.
///
/// The name is a leftover from when this view withheld the key. It withholds nothing now,
/// because the browser queries Linear itself and needs a key to do it. What remains of the
/// redaction is a direction: `PUT /api/config` treats `linear_key` as write-only, so a blank
/// submission keeps the stored key rather than clearing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct PresetRedacted {
    pub name: String,
    pub labels: Vec<String>,
    /// The preset's Linear personal API key, verbatim. Empty when none is configured, which
    /// is the whole test the settings panel runs to decide whether its write-only key input
    /// reads as already set.
    pub linear_key: String,
    /// Default account selections ([`Preset::claude_account`] / [`Preset::codex_account`]) —
    /// not secrets, shown verbatim.
    pub claude_account: String,
    pub codex_account: String,
    pub agent_playbook: String,
    pub global_prompt: String,
    pub startup_script: String,
    pub dockerfile: String,
}

/// A named pool of clone accounts (by email). A clone bound to a group sticks to its
/// account until that account exceeds the 5h usage cap (or leaves the group), then
/// moves to the group's least-loaded / least-used member — sticky, because an account
/// switch cold-starts the clone's prompt cache. Carries no secrets — just a name +
/// member emails — so it's TS-exported and shown verbatim in the redacted config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct CloneGroup {
    pub name: String,
    #[serde(default)]
    pub accounts: Vec<String>,
}

/// Docker backend settings for the clone fleet. No secrets — the local daemon is
/// reached over the unix socket, so (unlike the retired Proxmox SSH target) there is
/// nothing to redact; the whole struct passes through into [`AppConfigRedacted`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct DockerConfig {
    /// Prefix for derived clone hostnames/names, e.g. `pega-` → `pega-dev-123`. Sanitized
    /// to DNS-label-safe chars at use; blank in the UI keeps the stored value. Immediate
    /// (carried from the retired `proxmox.hostname_prefix`).
    #[serde(default = "default_hostname_prefix")]
    pub hostname_prefix: String,
    /// CPU limit per clone (`nano_cpus` = `clone_cpus * 1e9`), matching LXC parity.
    #[serde(default = "default_clone_cpus")]
    pub clone_cpus: u32,
    /// Memory limit per clone in MiB (+8 GiB swap), matching LXC parity.
    #[serde(default = "default_clone_memory_mb")]
    pub clone_memory_mb: u32,
    /// REMOVED `profile_lines`: presets carry their own full Dockerfile now.
    /// Template home seed snapshot (`<dataset>@<snap>`). A create clones the new home
    /// from it by default, so template clones start with content; empty means a fresh
    /// home. Seed refresh is manual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_snapshot: Option<String>,
    /// Parent ZFS dataset for all gen-2 clone homes (`<this>/<clone-id>`), mounted
    /// into the outer CT once at `/srv/rmng-homes`. Per-machine: the pool name differs
    /// per host (e.g. `tank/rmng/homes` vs `rpool/rmng/homes`). Immediate-apply (read
    /// fresh per zfs call); changing it does not move existing datasets.
    #[serde(default = "default_homes_parent")]
    pub homes_parent: String,
}

fn default_hostname_prefix() -> String {
    "pega-".into()
}
fn default_clone_cpus() -> u32 {
    16
}
fn default_clone_memory_mb() -> u32 {
    32768
}
fn default_homes_parent() -> String {
    "tank/rmng/homes".into()
}

fn default_preset_dockerfile() -> String {
    "FROM pegasis0/rmng-template:latest".into()
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            hostname_prefix: default_hostname_prefix(),
            clone_cpus: default_clone_cpus(),
            clone_memory_mb: default_clone_memory_mb(),
            seed_snapshot: None,
            homes_parent: default_homes_parent(),
        }
    }
}

/// FNV-1a 64 over bytes (stable across restarts, no new deps for `wire`).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Derived-image tag for gen-2 clones: `rmng-p-<16 hex>` over the preset's FULL
/// Dockerfile text. Same text twice means one build; same text NEVER rebuilds — a
/// base release under the same tag does not invalidate it. Refresh is manual: edit
/// the Dockerfile (any text change re-tags) or hit the preset's rebuild button.
/// Old tags purge when unused, never picked.
pub fn dockerfile_tag(dockerfile: &str) -> String {
    // Canonicalize so trivial formatting edits don't rebuild: trim trailing blank lines.
    format!("rmng-p-{:016x}", fnv1a64(dockerfile.trim_end().as_bytes()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ClaudeConfig {
    /// Account email pinned to the top of the usage list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_email: Option<String>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self { pinned_email: None }
    }
}

/// What settles the clones the file checks cannot: GPT, on an imported Codex account's
/// ChatGPT plan. See [`crate::MonitorState`].
///
/// No credential of its own. The server already holds that account's OAuth pair to run the
/// clones, and the judge spends the same weekly allowance they do. With no Codex account
/// imported, nothing answers the question and no clone is ever reported as working. That is
/// deliberate: a guess in either direction is worse than an honest "not working", and
/// per-clone token accounting is unaffected either way.
///
/// Nothing here is secret, so the whole struct reaches the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct JudgeConfig {
    /// Which GPT answers.
    #[serde(default = "default_codex_judge_model")]
    pub codex_model: String,
    /// Which imported Codex account pays for those calls. Unset means the first imported
    /// account, so a rig with one account needs no answer here. The calls come out of that
    /// account's weekly ChatGPT allowance, the same one its clones spend.
    ///
    /// `Option` for the same reason [`ClaudeConfig::pinned_email`] is one: a `PUT` reads an
    /// empty string as "keep what is stored", so `null` is how the panel says "no account in
    /// particular" once one has been picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_email: Option<String>,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            codex_model: default_codex_judge_model(),
            codex_email: None,
        }
    }
}

fn default_codex_judge_model() -> String {
    "gpt-5.6-luna".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct CodexConfig {
    /// Account email pinned to the top of the usage list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_email: Option<String>,
    /// When true, auto-spend one banked reset credit once every managed Codex account
    /// is over the weekly cap with no 7d reset within 24h (see `codex.rs` fleet gate).
    #[serde(default)]
    pub auto_reset: bool,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            pinned_email: None,
            auto_reset: false,
        }
    }
}

/// Full server config (with secrets). Loaded from `config.json`; serialized back
/// atomically at 0600. Not exported to TS — the browser only sees the redacted view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// Latched `true` by the first-run setup wizard once setup is complete; gates the
    /// frontend until then. The Proxmox-era grandfather rule is gone: an old `config.json`
    /// re-runs the wizard (new machine, no network / base image), so this stays `false`
    /// unless the wizard set it.
    #[serde(default)]
    pub setup_complete: bool,
    /// Named monitor-layout presets. The operator switches the active one from the
    /// sidebar (`POST /api/layout/activate`); the active preset drives `effective_monitors()`.
    #[serde(default)]
    pub layout_presets: Vec<LayoutPreset>,
    /// Name of the active layout preset (the fleet-wide live layout).
    #[serde(default)]
    pub active_layout: String,
    #[serde(default)]
    pub docker: DockerConfig,
    #[serde(default)]
    pub claude: ClaudeConfig,
    #[serde(default)]
    pub codex: CodexConfig,
    /// Named account pools a clone can be bound to for rotation (members are
    /// emails of imported accounts, from the server's `claude-accounts.json`).
    #[serde(default)]
    pub clone_groups: Vec<CloneGroup>,
    /// Named Codex account pools a clone can be bound to for rotation (members are emails
    /// of imported Codex accounts, from the server's `codex-accounts.json`).
    #[serde(default)]
    pub codex_groups: Vec<CloneGroup>,
    /// Clone presets (env vars + Linear key + auto-select ticket labels). Auto-selected
    /// by ticket label when cloning from a ticket; required pick otherwise.
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Chroma subsampling for the viewer video stream (default 4:2:0). Restart-required
    /// (the media plane's encode path is wired at startup).
    #[serde(default)]
    pub chroma: ChromaMode,
    /// SSH bastion access settings (jump host into clones). Non-secret — public keys pass
    /// through [`AppConfigRedacted`] intact.
    #[serde(default)]
    pub ssh: SshConfig,
    /// The desktop agent's base playbook (operating notes + ticket procedure), injected into
    /// each new clone at creation as its system-prompt append. Seeded with the shipped default
    /// (the wrapper's `agent-instructions.md`); edited in Settings. Applies to the next clone.
    /// (Layer **b**: node-agent extra, all presets.)
    #[serde(default = "default_agent_playbook")]
    pub agent_playbook: String,
    /// The global agent prompt every coding agent reads as its native operating memory
    /// (written to CLAUDE.md / `~/.codex/AGENTS.md`, and read
    /// by the node-agent via `settingSources:["user"]`). Seeded with the shipped default; edited
    /// in Settings. Kept in sync into existing clones by the reconciler. (Layer **a**: global
    /// prompt, all agents, all presets.) Keep desktop/Cursor procedure OUT of this — that belongs
    /// in [`agent_playbook`] (the inner Cursor Claude Code reads CLAUDE.md and would recurse).
    #[serde(default = "default_global_prompt")]
    pub global_prompt: String,
    /// Which GPT the stuck detector asks, and which Codex account pays for it.
    #[serde(default)]
    pub judge: JudgeConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            setup_complete: false,
            layout_presets: Vec::new(),
            active_layout: String::new(),
            docker: DockerConfig::default(),
            claude: ClaudeConfig::default(),
            codex: CodexConfig::default(),
            clone_groups: Vec::new(),
            codex_groups: Vec::new(),
            presets: Vec::new(),
            chroma: ChromaMode::default(),
            ssh: SshConfig::default(),
            agent_playbook: default_agent_playbook(),
            global_prompt: default_global_prompt(),
            judge: JudgeConfig::default(),
        }
    }
}

/// The shipped agent playbook: the wrapper's merged instructions file, embedded so the
/// control-server can seed the setting and inject it without a runtime file dependency.
/// Same file the agent-wrapper bakes in as its fallback (single source of truth).
fn default_agent_playbook() -> String {
    include_str!("../../../agent-wrapper/agent-instructions.md").to_string()
}
/// The shipped global agent prompt (layer **a** default): the shared "operating memory" every
/// coding agent reads as its native global rules. General engineering guidance only — the
/// desktop/Cursor ticket procedure lives in the node-agent playbook, never here. This is the
/// single source of truth for the body the control-server writes to CLAUDE.md / AGENTS.md.
pub fn default_global_prompt() -> String {
    "# Working in this clone\n\nThis machine is a **disposable, single-purpose dev sandbox** that belongs to you,\nwith **passwordless `sudo`**. Install packages, toolchains, and global CLIs freely\nand reconfigure the system as needed — the machine itself is throwaway and there is\nno other user to disturb. Optimize for getting the task done.\n\n## When you're blocked\n\nIf you're genuinely stuck — missing access or credentials, an ambiguous\nrequirement, or a call that's the human's to make — **stop and ask** rather than\nguessing or thrashing. A precise question beats a confident wrong turn.\n".to_string()
}

impl AppConfig {
    /// The active preset's monitors. Falls back to the first preset, then to a dual
    /// 2560×1440 side-by-side default (primary on the right) when no presets exist.
    pub fn effective_monitors(&self) -> Vec<MonitorSpec> {
        if let Some(p) = self
            .layout_presets
            .iter()
            .find(|p| p.name == self.active_layout)
        {
            return p.monitors.clone();
        }
        if let Some(p) = self.layout_presets.first() {
            return p.monitors.clone();
        }
        vec![
            MonitorSpec {
                width: 2560,
                height: 1440,
                x: 2560,
                y: 0,
                primary: true,
            },
            MonitorSpec {
                width: 2560,
                height: 1440,
                x: 0,
                y: 0,
                primary: false,
            },
        ]
    }

    /// Produce the redacted view for `GET /api/config`. Each preset's Linear key passes
    /// through verbatim; what the redaction still does is make it write-only on the way back.
    pub fn redacted(&self) -> AppConfigRedacted {
        AppConfigRedacted {
            setup_complete: self.setup_complete,
            layout_presets: self.layout_presets.clone(),
            active_layout: self.active_layout.clone(),
            docker: self.docker.clone(),
            claude: self.claude.clone(),
            codex: self.codex.clone(),
            clone_groups: self.clone_groups.clone(),
            codex_groups: self.codex_groups.clone(),
            presets: self.presets.iter().map(Preset::redacted).collect(),
            chroma: self.chroma,
            ssh: self.ssh.clone(),
            agent_playbook: self.agent_playbook.clone(),
            global_prompt: self.global_prompt.clone(),
            judge: self.judge.clone(),
        }
    }
}

/// The shape `GET /api/config` returns: the same structure as [`AppConfig`], each preset's
/// Linear key included verbatim, because the browser needs a key to query Linear with. For
/// those keys the redaction is a direction rather than a mask, and `PUT /api/config` takes
/// each one as write-only. Powers the Settings UI.
///
/// Nothing in it is withheld. Every credential the server holds is either a preset's Linear
/// key, which the browser needs, or an account token that lives in its own store rather than
/// in the config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct AppConfigRedacted {
    pub setup_complete: bool,
    pub layout_presets: Vec<LayoutPreset>,
    pub active_layout: String,
    pub docker: DockerConfig,
    pub claude: ClaudeConfig,
    pub codex: CodexConfig,
    pub clone_groups: Vec<CloneGroup>,
    pub codex_groups: Vec<CloneGroup>,
    pub presets: Vec<PresetRedacted>,
    pub chroma: ChromaMode,
    pub ssh: SshConfig,
    pub agent_playbook: String,
    pub global_prompt: String,
    /// Which GPT the stuck detector asks, and which Codex account pays for it.
    pub judge: JudgeConfig,
}

/// Response body for `PUT /api/config`: the redacted config after the merge, plus
/// whether the change touched a restart-required setting (the UI surfaces a restart
/// prompt when `restartRequired` is true).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ConfigPutResponse {
    pub config: AppConfigRedacted,
    pub restart_required: bool,
}

/// One row of the setup wizard's environment preflight (`GET /api/setup/env`): a named
/// check (Docker socket reachable, kernel features, etc.) with its pass/fail verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct EnvCheckRow {
    /// Stable machine id for the check (e.g. `dockerSocket`).
    pub id: String,
    /// Human-readable label shown in the wizard.
    pub label: String,
    /// Whether the check passed.
    pub ok: bool,
    /// Detail / diagnostic line shown under the label (empty when nothing to add).
    pub detail: String,
    /// Whether a failure blocks setup (vs. an advisory warning).
    pub required: bool,
}

/// Response body for `GET /api/setup/env`: the environment preflight rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct SetupEnv {
    pub rows: Vec<EnvCheckRow>,
}

/// A clone-source image (labeled `rmng.image=1`) as shown to the browser
/// (`GET /api/images`). Images replace the retired clone-id templates: any clone can be
/// committed to one, and clone creation picks from these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ImageInfo {
    /// Full image id (`sha256:…`).
    pub id: String,
    /// Repo tag reference, e.g. `pegasis0/rmng-template:latest`.
    pub reference: String,
    pub size_bytes: i64,
    /// ISO timestamp the image was created.
    pub created_at: String,
    /// True for the wizard-built base image (`rmng.base=1`).
    pub base: bool,
    /// Lineage: the reference this image was committed from (`rmng.created-from`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_from: Option<String>,
    /// Ids of live clones currently running on this image.
    pub in_use_by: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_tag_shape() {
        let tag = dockerfile_tag("FROM x:latest");
        assert!(tag.starts_with("rmng-p-"), "{tag}");
        assert_ne!(tag, dockerfile_tag("FROM x:latest\nRUN foo"));
        // Trailing blank lines do not re-tag.
        assert_eq!(tag, dockerfile_tag("FROM x:latest\n\n"));
    }

    #[test]
    fn defaults_are_sane() {
        // Missing keys fall back to the same defaults (older config.json stays valid).
        let d: AppConfig = serde_json::from_str("{}").unwrap();
        // Retired Advanced-pane keys in an old file are dropped, never an error.
        let old: AppConfig = serde_json::from_str(
            r#"{"listen":{"web":9000},"agentPort":4096,"dataDir":"data","staticDir":"","cloneSocket":"/srv/rmng-sock/clones.sock"}"#,
        )
        .unwrap();
        assert!(!old.setup_complete);
        assert!(!d.setup_complete);
        assert_eq!(DOCKER_SOCKET, "/var/run/docker.sock");
        assert_eq!(DOCKER_SUBNET, "10.99.0.0/24");
        assert_eq!(SERVER_IMAGE, "pegasis0/rmng:latest");
        assert!(BUILD_INFRA_ENABLED);
        assert_eq!(REGISTRY_IMAGE, "registry:2.8.3");
        assert_eq!(BUILDKIT_IMAGE, "moby/buildkit:v0.17.2");
        assert_eq!(BUILDKIT_CACHE_GB, 40);
        assert_eq!(CLAUDE_POLL_SECS, 600);
        assert_eq!(CODEX_POLL_SECS, 600);
        assert!(CODEX_USAGE_POLLING);
        let mons = AppConfig::default().effective_monitors();
        assert_eq!(mons.len(), 2);
        assert_eq!(
            (mons[0].width, mons[0].height, mons[0].x),
            (2560, 1440, 2560)
        );
        assert!(mons[0].primary);
        assert_eq!(mons[1].x, 0);
        assert!(!mons[1].primary);
    }

    #[test]
    fn docker_config_ignores_retired_keys() {
        // A config.json written before the hardcoding still loads: the retired keys
        // (socket, subnet, images, poll intervals, public host) are dropped, never an error.
        let json = r#"{
            "socket": "/var/run/docker.sock",
            "subnet": "10.99.0.0/24",
            "hostnamePrefix": "pega-",
            "cloneCpus": 16,
            "cloneMemoryMb": 32768,
            "templateReference": "pegasis0/rmng-template:latest",
            "serverImage": "pegasis0/rmng:latest",
            "buildInfraEnabled": false,
            "registryImage": "other",
            "buildkitImage": "other",
            "buildkitCacheGb": 1
        }"#;
        let cfg: DockerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.hostname_prefix, "pega-");
        assert_eq!(cfg.clone_cpus, 16);
    }

    #[test]
    fn chroma_mode_defaults_and_serde() {
        // Default is 4:2:0 (today's behavior / full capacity).
        assert_eq!(ChromaMode::default(), ChromaMode::Yuv420);
        assert_eq!(AppConfig::default().chroma, ChromaMode::Yuv420);
        // Wire/JSON representation is lowercase.
        assert_eq!(
            serde_json::to_string(&ChromaMode::Yuv420).unwrap(),
            "\"yuv420\""
        );
        assert_eq!(
            serde_json::to_string(&ChromaMode::Yuv444).unwrap(),
            "\"yuv444\""
        );
        // Missing field falls back to the default (older config.json stays valid).
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.chroma, ChromaMode::Yuv420);
        // Redaction passes chroma through (non-secret).
        let r = AppConfig {
            chroma: ChromaMode::Yuv444,
            ..Default::default()
        }
        .redacted();
        assert_eq!(r.chroma, ChromaMode::Yuv444);
    }

    #[test]
    fn preset_parses_with_serde_defaults() {
        // A minimal preset still parses; labels/linearKey/dockerfile default empty.
        // Retired keys (`vars`, `image`, `profileLines`) are ignored, so old files load.
        let c: AppConfig = serde_json::from_str(
            r#"{ "presets": [
                { "name": "min", "vars": [{ "key": "A", "value": "1" }] },
                { "name": "full", "labels": ["Frontend"], "linearKey": "K1", "dockerfile": "FROM x:y" }
            ] }"#,
        )
        .unwrap();
        assert_eq!(c.presets.len(), 2);
        assert!(c.presets[0].labels.is_empty() && c.presets[0].linear_key.is_empty());
        assert_eq!(
            c.presets[0].dockerfile,
            "FROM pegasis0/rmng-template:latest"
        );
        assert_eq!(c.presets[1].labels, vec!["Frontend"]);
        assert_eq!(c.presets[1].linear_key, "K1");
        // Round-trips as camelCase.
        let v = serde_json::to_value(&c.presets[1]).unwrap();
        assert_eq!(v["linearKey"], "K1");
        // Missing field → empty list.
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(c.presets.is_empty());
    }

    #[test]
    fn codex_config_defaults_and_passthrough() {
        // Defaults: no pinned email, no auto-reset. Retired poll keys in JSON are dropped.
        let c = AppConfig::default();
        assert!(c.codex.pinned_email.is_none());
        assert!(!c.codex.auto_reset, "auto_reset defaults to false");
        let off: AppConfig = serde_json::from_str(
            r#"{ "codex": { "pollSecs": 300, "usagePolling": false, "autoReset": true } }"#,
        )
        .unwrap();
        assert!(off.codex.auto_reset, "autoReset parses from camelCase JSON");
        // Redaction passes codex through (non-secret).
        let r = AppConfig {
            codex: CodexConfig {
                auto_reset: true,
                ..Default::default()
            },
            ..Default::default()
        }
        .redacted();
        assert!(r.codex.auto_reset);
        // Round-trips as camelCase.
        let v = serde_json::to_value(&CodexConfig::default()).unwrap();
        assert!(v.get("autoReset").is_some());
    }

    /// The redacted view vends the Linear key rather than hiding it: the browser calls Linear
    /// itself, so `GET /api/config` is where it gets a key.
    #[test]
    fn redaction_vends_the_linear_key() {
        let c = AppConfig {
            setup_complete: true,
            docker: DockerConfig {
                hostname_prefix: "dev-".into(),
                ..Default::default()
            },
            presets: vec![
                Preset {
                    name: "med".into(),
                    labels: vec!["Backend".into()],
                    linear_key: "lin_api_secret".into(),
                    claude_account: "group:pooled".into(),
                    codex_account: String::new(),
                    agent_playbook: String::new(),
                    global_prompt: String::new(),
                    startup_script: String::new(),
                    dockerfile: "FROM x:latest".into(),
                },
                Preset {
                    name: "bare".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let r = c.redacted();
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("10.0.0.100"));
        assert!(json.contains("lin_api_secret"), "the key is vended: {json}");
        assert_eq!(r.presets.len(), 2);
        assert_eq!(r.presets[0].linear_key, "lin_api_secret");
        assert_eq!(r.presets[0].name, "med");
        assert_eq!(r.presets[0].labels, vec!["Backend"]); // labels pass through
        assert_eq!(r.presets[0].dockerfile, "FROM x:latest");
        // Account defaults are not secrets — they pass through the redaction verbatim.
        assert_eq!(r.presets[0].claude_account, "group:pooled");
        assert_eq!(r.presets[0].codex_account, "");
        // A preset with no key configured reads back empty, which is what the settings
        // panel's write-only key input tests to show itself as unset.
        assert_eq!(r.presets[1].linear_key, "");
        // Non-secret fields pass through verbatim; the Docker backend has no secret.
        assert!(r.setup_complete);
        assert_eq!(r.docker.hostname_prefix, "dev-");
    }

    /// Which provider answers is a choice rather than a credential, so it passes through
    /// whole. A config written before the setting existed reads back as OpenRouter, which is
    /// what those rigs were already doing.
    #[test]
    fn the_judge_choice_survives_the_redaction() {
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.judge.codex_model, "gpt-5.6-luna");
        assert_eq!(c.judge.codex_email, None);

        let c = AppConfig {
            judge: JudgeConfig {
                codex_email: Some("alex@example.com".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let r = c.redacted();
        assert_eq!(r.judge.codex_email.as_deref(), Some("alex@example.com"));
        let v = serde_json::to_value(&r.judge).unwrap();
        assert!(
            v.get("codexModel").is_some(),
            "round-trips as camelCase: {v}"
        );
    }

    #[test]
    fn agent_playbook_defaults_to_embedded_file() {
        // Missing key ⇒ the shipped default (the merged wrapper instructions), non-empty.
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(!c.agent_playbook.is_empty());
        assert_eq!(c.agent_playbook, default_agent_playbook());
        // A preset's playbook defaults to empty (optional append).
        let p: Preset = serde_json::from_str(r#"{ "name": "x" }"#).unwrap();
        assert!(p.agent_playbook.is_empty());
    }

    #[test]
    fn global_prompt_defaults_to_shipped_body() {
        // Missing key ⇒ the shipped shared operating-memory body (non-empty).
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(!c.global_prompt.is_empty());
        assert_eq!(c.global_prompt, default_global_prompt());
        assert!(c.global_prompt.contains("disposable"));
        // A preset's global prompt defaults to empty (optional append, layer c).
        let p: Preset = serde_json::from_str(r#"{ "name": "x" }"#).unwrap();
        assert!(p.global_prompt.is_empty());
    }

    #[test]
    fn agent_playbook_passes_through_redaction() {
        let c = AppConfig {
            agent_playbook: "GLOBAL NOTES".into(),
            global_prompt: "GLOBAL PROMPT".into(),
            presets: vec![Preset {
                name: "p".into(),
                agent_playbook: "PRESET APPEND".into(),
                global_prompt: "PRESET PROMPT".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = c.redacted();
        assert_eq!(r.agent_playbook, "GLOBAL NOTES");
        assert_eq!(r.global_prompt, "GLOBAL PROMPT");
        assert_eq!(r.presets[0].agent_playbook, "PRESET APPEND");
        assert_eq!(r.presets[0].global_prompt, "PRESET PROMPT");
    }

    #[test]
    fn effective_monitors_selection_rules() {
        // Active preset wins.
        let mut c = AppConfig::default();
        c.layout_presets = vec![
            LayoutPreset {
                name: "A".into(),
                monitors: vec![MonitorSpec {
                    width: 1920,
                    height: 1080,
                    x: 0,
                    y: 0,
                    primary: true,
                }],
            },
            LayoutPreset {
                name: "B".into(),
                monitors: vec![MonitorSpec {
                    width: 3840,
                    height: 2160,
                    x: 0,
                    y: 0,
                    primary: true,
                }],
            },
        ];
        c.active_layout = "B".into();
        assert_eq!(c.effective_monitors(), c.layout_presets[1].monitors);
        // A missing active name falls back to the first preset.
        c.active_layout = "Nonexistent".into();
        assert_eq!(c.effective_monitors(), c.layout_presets[0].monitors);
        // No presets → dual-1440p default (unchanged behavior).
        let c = AppConfig::default();
        assert_eq!(c.effective_monitors().len(), 2);
        assert!(c.effective_monitors()[0].primary);
    }

    #[test]
    fn app_config_ssh_round_trips_camel_case() {
        let mut c = AppConfig::default();
        c.ssh = SshConfig {
            authorized_keys: vec!["ssh-ed25519 AAAA me@laptop".into()],
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(
            json.contains("\"authorizedKeys\""),
            "camelCase key missing: {json}"
        );
        // A retired publicHost key in an old file is dropped, never an error.
        let back: AppConfig =
            serde_json::from_str(r#"{"ssh":{"publicHost":"rmng.example.com"}}"#).unwrap();
        assert!(back.ssh.authorized_keys.is_empty());
    }

    #[test]
    fn redacted_carries_ssh_keys_unredacted() {
        // Public keys are not secret — they must survive redaction (the UI needs them).
        let mut c = AppConfig::default();
        c.ssh.authorized_keys = vec!["ssh-ed25519 AAAA me@laptop".into()];
        let r = c.redacted();
        assert_eq!(r.ssh.authorized_keys, c.ssh.authorized_keys);
    }
}

#[cfg(test)]
mod port_tests {
    use super::*;

    #[test]
    fn hardcoded_ports_match_the_old_defaults() {
        assert_eq!(PORT_WEB, 9000);
        assert_eq!(PORT_VIDEO, 9001);
        assert_eq!(PORT_DAEMON_MCP, 9004);
        assert_eq!(PORT_FORWARD, 9005);
        assert_eq!(PORT_BASTION, 2222);
        assert_eq!(AGENT_PORT, 4096);
        assert_eq!(DATA_DIR, "data");
        assert_eq!(CLONE_SOCKET, "/srv/rmng-sock/clones.sock");
    }
}
