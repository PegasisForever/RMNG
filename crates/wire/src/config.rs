//! `AppConfig` — every setting, edited via the Settings UI (no hand-edited files).
//!
//! Secrets (preset Linear keys) live only in the server's `config.json` (0600) and are
//! **never** placed in `ControlState` or sent to the browser. `GET /api/config` returns
//! [`AppConfigRedacted`] (secrets shown as set/unset); `PUT /api/config` takes write-only
//! secret fields. The Docker backend has no secret (local unix socket), so
//! [`DockerConfig`] passes through the redacted view intact.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::control::{LayoutPreset, MonitorSpec};

/// The control-server listen ports: video, web, per-clone MCP, global/fleet MCP, the
/// in-clone daemon MCP, and the forward data plane (see README).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ListenConfig {
    pub web: u16,
    pub video: u16,
    pub clone_mcp: u16,
    pub global_mcp: u16,
    /// The clone-daemon's in-clone HTTP MCP port. The fleet MCP proxies desktop/window
    /// tools to `http://{clone-ip}:{daemon_mcp}`; each clone-daemon listens here (set via
    /// `RMNG_DAEMON_MCP_PORT`). Same value for every clone.
    #[serde(default = "default_daemon_mcp")]
    pub daemon_mcp: u16,
    /// The control-server's port-forward data plane. The viewer opens one TCP
    /// connection here per accepted local socket; the server splices to the clone.
    /// Restart-required (bound at startup).
    #[serde(default = "default_forward")]
    pub forward: u16,
}

fn default_daemon_mcp() -> u16 {
    9004
}

fn default_forward() -> u16 {
    9005
}

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

impl Default for ListenConfig {
    fn default() -> Self {
        Self { web: 9000, video: 9001, clone_mcp: 9002, global_mcp: 9003, daemon_mcp: default_daemon_mcp(), forward: default_forward() }
    }
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

/// A clone preset: a Linear identity (API key + the ticket labels that auto-select
/// this preset when cloning from a ticket) plus a named set of environment variables,
/// applied to a clone's session at creation (written to
/// `~/.config/environment.d/30-rmng-preset.conf`; the Linear key is additionally
/// injected as `LINEAR_API_KEY`, which auths the clone's `linear` MCP). Vars that must
/// ALWAYS be present (e.g. `XDG_CURRENT_DESKTOP`) are NOT presets — they're baked into the
/// template's base session env by `template/setup/30-user.sh` at template build, inherited by
/// every clone.
/// NOT TS-exported: `linear_key` is a secret — the browser sees [`PresetRedacted`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub name: String,
    /// Linear ticket labels that auto-select this preset (matched case-insensitively
    /// against the ticket's labels; first matching preset in config order wins).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Linear personal API key (**secret**; injected into clones as `LINEAR_API_KEY`).
    #[serde(default)]
    pub linear_key: String,
    #[serde(default)]
    pub vars: Vec<EnvVar>,
    /// Optional per-preset text appended (after `"\n\n"`) to the global agent playbook for
    /// clones of this preset. Empty ⇒ no append. Non-secret.
    #[serde(default)]
    pub agent_playbook: String,
}

impl Preset {
    pub fn redacted(&self) -> PresetRedacted {
        PresetRedacted {
            name: self.name.clone(),
            labels: self.labels.clone(),
            linear_key_set: !self.linear_key.is_empty(),
            vars: self.vars.clone(),
            agent_playbook: self.agent_playbook.clone(),
        }
    }
}

/// A preset as shown to the browser: everything but the Linear key, which is
/// replaced by a "is set" flag (write-only secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct PresetRedacted {
    pub name: String,
    pub labels: Vec<String>,
    pub linear_key_set: bool,
    pub vars: Vec<EnvVar>,
    pub agent_playbook: String,
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
    /// Docker daemon unix socket the control-server drives clones through, e.g.
    /// `/var/run/docker.sock`. **Restart-required**: the bollard client is built at
    /// startup.
    #[serde(default = "default_docker_socket")]
    pub socket: String,
    /// CIDR for the user-defined `rmng` bridge network (`.1` gateway, `.2` control-server,
    /// `.10+` clone pool). **One-time**: baked into the network + every clone's static IP
    /// at first-run setup (validated `/16`–`/24` at config merge).
    #[serde(default = "default_docker_subnet")]
    pub subnet: String,
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
    /// Registry reference the setup wizard pulls the clone template from. The pulled image
    /// keeps this `repo:tag` as its clone-source reference (no local retag), so it's also
    /// exactly what the image picker lists and what clones are created FROM. Immediate-apply
    /// (read fresh per pull); no secret (public image over the local daemon), so it passes
    /// through the redacted view.
    #[serde(default = "default_template_reference")]
    pub template_reference: String,
    /// Registry reference the in-product self-update pulls the control-server image from
    /// (and digest-compares against for update-available detection). Immediate-apply (read
    /// fresh per check/update); no secret (public image over the local daemon), so it
    /// passes through the redacted view.
    #[serde(default = "default_server_image")]
    pub server_image: String,
}

fn default_docker_socket() -> String {
    "/var/run/docker.sock".into()
}
fn default_docker_subnet() -> String {
    "10.99.0.0/24".into()
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
fn default_template_reference() -> String {
    "pegasis0/rmng-template:latest".into()
}
fn default_server_image() -> String {
    "pegasis0/rmng:latest".into()
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            socket: default_docker_socket(),
            subnet: default_docker_subnet(),
            hostname_prefix: default_hostname_prefix(),
            clone_cpus: default_clone_cpus(),
            clone_memory_mb: default_clone_memory_mb(),
            template_reference: default_template_reference(),
            server_image: default_server_image(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ClaudeConfig {
    /// Usage poll interval (seconds, floored at 15).
    pub poll_secs: u64,
    /// Account email pinned to the top of the usage list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_email: Option<String>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self { poll_secs: 600, pinned_email: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct CodexConfig {
    /// Usage poll interval (seconds, floored at 15 by the poller).
    pub poll_secs: u64,
    /// Account email pinned to the top of the usage list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_email: Option<String>,
    /// Poll the ChatGPT usage endpoint. When false, the poller still refreshes + pushes
    /// tokens and publishes base views (with an explanatory `error`), but skips the usage
    /// fetch — an escape hatch if the unofficial `/wham/usage` shape drifts.
    #[serde(default = "default_true")]
    pub usage_polling: bool,
}

fn default_true() -> bool {
    true
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self { poll_secs: 600, pinned_email: None, usage_polling: true }
    }
}

/// Full server config (with secrets). Loaded from `config.json`; serialized back
/// atomically at 0600. Not exported to TS — the browser only sees the redacted view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub listen: ListenConfig,
    /// agent-wrapper port on each clone (chat proxy + reload nudge).
    #[serde(default = "default_agent_port")]
    pub agent_port: u16,
    /// Data directory (state.json, chats, uploads, hosts mounts, secrets).
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// Built frontend bundle directory served on the web port. Empty (the default) serves
    /// the installed frontend (`/usr/local/share/rmng/static` in the image, else the repo
    /// dev build); a non-empty path overrides it (dev hot-reload without a rebuild).
    /// Restart-required (the static-file service is wired at startup).
    #[serde(default = "default_static_dir")]
    pub static_dir: String,
    /// Unix socket the clone-daemons connect to (media plane over `SCM_RIGHTS`, not the
    /// network). **One-time**: baked into every CT's socket bind-mount and clone-daemon
    /// unit (`RMNG_SOCKET`) at provision, so it can only be set during first-run setup
    /// (changing it later wouldn't update already-provisioned CTs). Also restart-required
    /// for pre-latch edits, since the server binds it at startup.
    #[serde(default = "default_clone_socket")]
    pub clone_socket: String,
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
    /// Vision-LLM inference server the needs-human detector (`clone-daemon wait-for-stuck`)
    /// polls — OpenAI-compatible `/v1/chat/completions`. Injected into each clone as
    /// `RMNG_INFERENCE_URL` at clone time. External infra the control-server can't
    /// auto-detect, so it's configured here (the old compiled-in default pointed at the
    /// retired stack's subnet address, unreachable from vmbr0 clones).
    #[serde(default = "default_inference_url")]
    pub detector_inference_url: String,
    /// The desktop agent's base playbook (operating notes + ticket procedure), injected into
    /// each new clone at creation as its system-prompt append. Seeded with the shipped default
    /// (the wrapper's `agent-instructions.md`); edited in Settings. Applies to the next clone.
    #[serde(default = "default_agent_playbook")]
    pub agent_playbook: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen: ListenConfig::default(),
            agent_port: default_agent_port(),
            data_dir: default_data_dir(),
            static_dir: default_static_dir(),
            clone_socket: default_clone_socket(),
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
            detector_inference_url: default_inference_url(),
            agent_playbook: default_agent_playbook(),
        }
    }
}

fn default_agent_port() -> u16 {
    4096
}
/// The shipped agent playbook: the wrapper's merged instructions file, embedded so the
/// control-server can seed the setting and inject it without a runtime file dependency.
/// Same file the agent-wrapper bakes in as its fallback (single source of truth).
fn default_agent_playbook() -> String {
    include_str!("../../../agent-wrapper/agent-instructions.md").to_string()
}
fn default_inference_url() -> String {
    "http://10.0.0.42:8080".into()
}
fn default_data_dir() -> String {
    "data".into()
}
fn default_static_dir() -> String {
    String::new()
}
fn default_clone_socket() -> String {
    "/srv/rmng-sock/clones.sock".into()
}

impl AppConfig {
    /// The active preset's monitors. Falls back to the first preset, then to a dual
    /// 2560×1440 side-by-side default (primary on the right) when no presets exist.
    pub fn effective_monitors(&self) -> Vec<MonitorSpec> {
        if let Some(p) = self.layout_presets.iter().find(|p| p.name == self.active_layout) {
            return p.monitors.clone();
        }
        if let Some(p) = self.layout_presets.first() {
            return p.monitors.clone();
        }
        vec![
            MonitorSpec { width: 2560, height: 1440, x: 2560, y: 0, primary: true },
            MonitorSpec { width: 2560, height: 1440, x: 0, y: 0, primary: false },
        ]
    }

    /// Produce the redacted view for `GET /api/config` (no plaintext secrets).
    pub fn redacted(&self) -> AppConfigRedacted {
        AppConfigRedacted {
            listen: self.listen,
            agent_port: self.agent_port,
            data_dir: self.data_dir.clone(),
            static_dir: self.static_dir.clone(),
            clone_socket: self.clone_socket.clone(),
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
            detector_inference_url: self.detector_inference_url.clone(),
            agent_playbook: self.agent_playbook.clone(),
        }
    }
}

/// The shape `GET /api/config` returns: same structure as [`AppConfig`] but with
/// every secret replaced by a boolean "is set". Powers the Settings UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct AppConfigRedacted {
    pub listen: ListenConfig,
    pub agent_port: u16,
    pub data_dir: String,
    pub static_dir: String,
    pub clone_socket: String,
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
    pub detector_inference_url: String,
    pub agent_playbook: String,
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
/// (`GET /api/images`). Images replace the retired host-id templates: any clone can be
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
    /// Host ids of live clones currently running on this image.
    pub in_use_by: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = AppConfig::default();
        assert_eq!(c.listen.web, 9000);
        assert_eq!(c.listen.video, 9001);
        assert_eq!(c.agent_port, 4096);
        // New one-time / restart-required fields carry their documented defaults.
        assert_eq!(c.static_dir, ""); // empty = installed/default frontend dir
        assert_eq!(c.clone_socket, "/srv/rmng-sock/clones.sock");
        assert!(!c.setup_complete); // wizard latches this true
        assert_eq!(c.docker.socket, "/var/run/docker.sock");
        assert_eq!(c.docker.subnet, "10.99.0.0/24");
        assert_eq!(c.docker.hostname_prefix, "pega-");
        assert_eq!(c.docker.clone_cpus, 16);
        assert_eq!(c.docker.clone_memory_mb, 32768);
        assert_eq!(c.docker.template_reference, "pegasis0/rmng-template:latest");
        // Missing keys fall back to the same defaults (older config.json stays valid).
        let d: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(d.static_dir, "");
        assert_eq!(d.clone_socket, "/srv/rmng-sock/clones.sock");
        assert!(!d.setup_complete);
        assert_eq!(d.docker.socket, "/var/run/docker.sock");
        assert_eq!(d.docker.subnet, "10.99.0.0/24");
        assert_eq!(d.docker.template_reference, "pegasis0/rmng-template:latest");
        let mons = c.effective_monitors();
        assert_eq!(mons.len(), 2);
        assert_eq!((mons[0].width, mons[0].height, mons[0].x), (2560, 1440, 2560));
        assert!(mons[0].primary);
        assert_eq!(mons[1].x, 0);
        assert!(!mons[1].primary);
    }

    #[test]
    fn docker_config_default_server_image() {
        let d = DockerConfig::default();
        assert_eq!(d.server_image, "pegasis0/rmng:latest");
    }

    #[test]
    fn chroma_mode_defaults_and_serde() {
        // Default is 4:2:0 (today's behavior / full capacity).
        assert_eq!(ChromaMode::default(), ChromaMode::Yuv420);
        assert_eq!(AppConfig::default().chroma, ChromaMode::Yuv420);
        // Wire/JSON representation is lowercase.
        assert_eq!(serde_json::to_string(&ChromaMode::Yuv420).unwrap(), "\"yuv420\"");
        assert_eq!(serde_json::to_string(&ChromaMode::Yuv444).unwrap(), "\"yuv444\"");
        // Missing field falls back to the default (older config.json stays valid).
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.chroma, ChromaMode::Yuv420);
        // Redaction passes chroma through (non-secret).
        let r = AppConfig { chroma: ChromaMode::Yuv444, ..Default::default() }.redacted();
        assert_eq!(r.chroma, ChromaMode::Yuv444);
    }

    #[test]
    fn preset_parses_with_serde_defaults() {
        // A minimal preset (older env-preset shape: just name + vars) still parses;
        // labels/linearKey default empty.
        let c: AppConfig = serde_json::from_str(
            r#"{ "presets": [
                { "name": "min", "vars": [{ "key": "A", "value": "1" }] },
                { "name": "full", "labels": ["Frontend"], "linearKey": "K1", "vars": [] }
            ] }"#,
        )
        .unwrap();
        assert_eq!(c.presets.len(), 2);
        assert!(c.presets[0].labels.is_empty() && c.presets[0].linear_key.is_empty());
        assert_eq!(c.presets[0].vars[0].key, "A");
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
        // Defaults: 600s poll, no pinned email, usage polling ON.
        let c = AppConfig::default();
        assert_eq!(c.codex.poll_secs, 600);
        assert!(c.codex.pinned_email.is_none());
        assert!(c.codex.usage_polling, "usage_polling defaults to true");
        assert!(c.codex_groups.is_empty());
        // Missing keys fall back to defaults (older config.json stays valid).
        let d: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(d.codex.poll_secs, 600);
        assert!(d.codex.usage_polling);
        assert!(d.codex_groups.is_empty());
        // usage_polling can be turned off from JSON (camelCase).
        let off: AppConfig =
            serde_json::from_str(r#"{ "codex": { "pollSecs": 300, "usagePolling": false } }"#).unwrap();
        assert_eq!(off.codex.poll_secs, 300);
        assert!(!off.codex.usage_polling);
        // Redaction passes codex + codex_groups through (non-secret).
        let r = AppConfig {
            codex: CodexConfig { poll_secs: 120, usage_polling: false, ..Default::default() },
            codex_groups: vec![CloneGroup { name: "g".into(), accounts: vec!["z@o".into()] }],
            ..Default::default()
        }
        .redacted();
        assert_eq!(r.codex.poll_secs, 120);
        assert!(!r.codex.usage_polling);
        assert_eq!(r.codex_groups.len(), 1);
        // Round-trips as camelCase.
        let v = serde_json::to_value(&CodexConfig::default()).unwrap();
        assert!(v.get("usagePolling").is_some());
    }

    #[test]
    fn redaction_hides_secrets() {
        let c = AppConfig {
            clone_socket: "/srv/rmng-sock/clones.sock".into(),
            setup_complete: true,
            docker: DockerConfig {
                subnet: "10.42.0.0/24".into(),
                hostname_prefix: "dev-".into(),
                template_reference: "pegasis0/rmng-template:v9".into(),
                ..Default::default()
            },
            presets: vec![
                Preset {
                    name: "med".into(),
                    labels: vec!["Backend".into()],
                    linear_key: "lin_api_secret".into(),
                    vars: vec![EnvVar { key: "A".into(), value: "1".into() }],
                    agent_playbook: String::new(),
                },
                Preset { name: "bare".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let r = c.redacted();
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("10.0.0.100"));
        assert!(!json.contains("lin_api_secret"));
        assert_eq!(r.presets.len(), 2);
        assert!(r.presets[0].linear_key_set && r.presets[0].name == "med");
        assert_eq!(r.presets[0].labels, vec!["Backend"]); // labels/vars pass through
        assert_eq!(r.presets[0].vars.len(), 1);
        assert!(!r.presets[1].linear_key_set);
        // Non-secret fields pass through verbatim; the Docker backend has no secret.
        assert_eq!(r.clone_socket, "/srv/rmng-sock/clones.sock");
        assert!(r.setup_complete);
        assert_eq!(r.docker.subnet, "10.42.0.0/24");
        assert_eq!(r.docker.hostname_prefix, "dev-");
        // template_reference is non-secret — it passes through the redacted view intact.
        assert_eq!(r.docker.template_reference, "pegasis0/rmng-template:v9");
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
    fn agent_playbook_passes_through_redaction() {
        let c = AppConfig {
            agent_playbook: "GLOBAL NOTES".into(),
            presets: vec![Preset {
                name: "p".into(),
                agent_playbook: "PRESET APPEND".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = c.redacted();
        assert_eq!(r.agent_playbook, "GLOBAL NOTES");
        assert_eq!(r.presets[0].agent_playbook, "PRESET APPEND");
    }

    #[test]
    fn effective_monitors_from_active_preset() {
        let mut c = AppConfig::default();
        c.layout_presets = vec![
            LayoutPreset { name: "A".into(), monitors: vec![
                MonitorSpec { width: 1920, height: 1080, x: 0, y: 0, primary: true }] },
            LayoutPreset { name: "B".into(), monitors: vec![
                MonitorSpec { width: 3840, height: 2160, x: 0, y: 0, primary: true }] },
        ];
        c.active_layout = "B".into();
        assert_eq!(c.effective_monitors(), c.layout_presets[1].monitors);
    }

    #[test]
    fn effective_monitors_defaults_when_empty() {
        // No presets → dual-1440p default (unchanged behavior).
        let c = AppConfig::default();
        assert_eq!(c.effective_monitors().len(), 2);
        assert!(c.effective_monitors()[0].primary);
    }

    #[test]
    fn effective_monitors_falls_back_to_first_when_active_missing() {
        let mut c = AppConfig::default();
        c.layout_presets = vec![LayoutPreset {
            name: "Only".into(),
            monitors: vec![MonitorSpec { width: 1280, height: 720, x: 0, y: 0, primary: true }],
        }];
        c.active_layout = "Nonexistent".into();
        assert_eq!(c.effective_monitors(), c.layout_presets[0].monitors);
    }
}

#[cfg(test)]
mod listen_tests {
    use super::*;

    #[test]
    fn listen_config_forward_defaults_9005() {
        assert_eq!(ListenConfig::default().forward, 9005);
        // absent in JSON → default
        let lc: ListenConfig =
            serde_json::from_str(r#"{"web":9000,"video":9001,"cloneMcp":9002,"globalMcp":9003}"#)
                .unwrap();
        assert_eq!(lc.forward, 9005);
    }
}
