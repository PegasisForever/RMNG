//! `AppConfig` — every setting, edited via the Settings UI (no hand-edited files).
//!
//! Secrets (proxmox ssh target, preset Linear keys) live only in the server's
//! `config.json` (0600) and are **never** placed in `ControlState`
//! or sent to the browser. `GET /api/config` returns [`AppConfigRedacted`]
//! (secrets shown as set/unset); `PUT /api/config` takes write-only secret fields.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::control::MonitorSpec;

/// The four listen ports (see README: 1 video, 2 web, 3 per-clone MCP, 4 global MCP).
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
}

fn default_daemon_mcp() -> u16 {
    9004
}

/// Chroma subsampling mode for the port-1 viewer video stream.
///
/// `Yuv420` is today's hardware path (one `W×H` NV12 H.264 stream per monitor).
/// `Yuv444` recovers full chroma using the RDP **AVC444** packing carried in a single
/// double-height `W×2H` stream (main view stacked over an auxiliary chroma view),
/// reassembled to 4:4:4 on the GPU at the viewer. Server-wide, chosen at launch
/// (`config.chroma` or the `RMNG_CHROMA` env override); the viewer learns the active
/// mode from the port-1 connect handshake.
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

impl ChromaMode {
    /// Parse the `RMNG_CHROMA` env value (`yuv420`/`420`, `yuv444`/`444`); `None` if unset/unknown.
    pub fn from_env_value(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "yuv420" | "420" | "i420" | "nv12" => Some(Self::Yuv420),
            "yuv444" | "444" | "avc444" => Some(Self::Yuv444),
            _ => None,
        }
    }
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self { web: 9000, video: 9001, clone_mcp: 9002, global_mcp: 9003, daemon_mcp: default_daemon_mcp() }
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
/// template's base session env by `provision-clone.sh`, inherited by every clone.
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
}

impl Preset {
    pub fn redacted(&self) -> PresetRedacted {
        PresetRedacted {
            name: self.name.clone(),
            labels: self.labels.clone(),
            linear_key_set: !self.linear_key.is_empty(),
            vars: self.vars.clone(),
        }
    }
}

/// A preset as shown to the browser: everything but the Linear key, which is
/// replaced by a "is set" flag (write-only secret, like the proxmox ssh target).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct PresetRedacted {
    pub name: String,
    pub labels: Vec<String>,
    pub linear_key_set: bool,
    pub vars: Vec<EnvVar>,
}

/// A named pool of clone accounts (by email). A clone bound to a group has its
/// account rotated among the group's members every cycle (by 5h usage). Carries no
/// secrets — just a name + member emails — so it's TS-exported and shown verbatim in
/// the redacted config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct CloneGroup {
    pub name: String,
    #[serde(default)]
    pub accounts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxmoxConfig {
    /// SSH target for the Proxmox node, e.g. `root@10.0.0.100` (secret-ish).
    #[serde(default)]
    pub ssh: String,
    /// OUI prefix for freshly-generated clone MACs, e.g. `BC:24:11`. A CoW clone inherits
    /// the template's MAC, so `clone.sh` regenerates one with this prefix to avoid a
    /// collision on the shared bridge. Config-only (not surfaced in the Settings UI).
    #[serde(default = "default_mac_prefix")]
    pub mac_prefix: String,
    /// Prefix for derived clone hostnames, e.g. `pega-` → `pega-dev-123` / `pega-my-task`.
    /// Sanitized to DNS-label-safe chars at use; blank in the UI keeps the stored value.
    #[serde(default = "default_hostname_prefix")]
    pub hostname_prefix: String,
}

fn default_mac_prefix() -> String {
    "BC:24:11".into()
}
fn default_hostname_prefix() -> String {
    "pega-".into()
}

impl Default for ProxmoxConfig {
    fn default() -> Self {
        Self {
            ssh: String::new(),
            mac_prefix: default_mac_prefix(),
            hostname_prefix: default_hostname_prefix(),
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
    /// Hot-swap a clone to another account when its usage is exhausted.
    #[serde(default)]
    pub auto_swap_on_exhaustion: bool,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self { poll_secs: 600, pinned_email: None, auto_swap_on_exhaustion: false }
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
    /// Built frontend bundle directory served on the web port.
    #[serde(default = "default_static_dir")]
    pub static_dir: String,
    #[serde(default)]
    pub monitors: Vec<MonitorSpec>,
    #[serde(default)]
    pub proxmox: ProxmoxConfig,
    #[serde(default)]
    pub claude: ClaudeConfig,
    /// Named account pools a clone can be bound to for rotation (members are
    /// emails of imported accounts, from the server's `claude-accounts.json`).
    #[serde(default)]
    pub clone_groups: Vec<CloneGroup>,
    /// Clone presets (env vars + Linear key + auto-select ticket labels). Auto-selected
    /// by ticket label when cloning from a ticket; required pick otherwise.
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Chroma subsampling for the viewer video stream (default 4:2:0). The `RMNG_CHROMA`
    /// env var overrides this at load time.
    #[serde(default)]
    pub chroma: ChromaMode,
    /// Vision-LLM inference server the needs-human detector (`clone-daemon wait-for-stuck`)
    /// polls — OpenAI-compatible `/v1/chat/completions`. Injected into each clone as
    /// `RMNG_INFERENCE_URL` at clone time. External infra the control-server can't
    /// auto-detect, so it's configured here (the old compiled-in default pointed at the
    /// retired stack's subnet address, unreachable from vmbr0 clones).
    #[serde(default = "default_inference_url")]
    pub detector_inference_url: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen: ListenConfig::default(),
            agent_port: default_agent_port(),
            data_dir: default_data_dir(),
            static_dir: default_static_dir(),
            monitors: Vec::new(),
            proxmox: ProxmoxConfig::default(),
            claude: ClaudeConfig::default(),
            clone_groups: Vec::new(),
            presets: Vec::new(),
            chroma: ChromaMode::default(),
            detector_inference_url: default_inference_url(),
        }
    }
}

fn default_agent_port() -> u16 {
    4096
}
fn default_inference_url() -> String {
    "http://10.0.0.42:8080".into()
}
fn default_data_dir() -> String {
    "data".into()
}
fn default_static_dir() -> String {
    "frontend/build/client".into()
}

impl AppConfig {
    /// Default monitor layout if none configured: dual 2560×1440 side-by-side,
    /// primary on the right (monitor 0 at x=2560, monitor 1 at x=0).
    pub fn effective_monitors(&self) -> Vec<MonitorSpec> {
        if self.monitors.is_empty() {
            vec![
                MonitorSpec { width: 2560, height: 1440, x: 2560, y: 0, primary: true },
                MonitorSpec { width: 2560, height: 1440, x: 0, y: 0, primary: false },
            ]
        } else {
            self.monitors.clone()
        }
    }

    /// Produce the redacted view for `GET /api/config` (no plaintext secrets).
    pub fn redacted(&self) -> AppConfigRedacted {
        AppConfigRedacted {
            listen: self.listen,
            agent_port: self.agent_port,
            data_dir: self.data_dir.clone(),
            static_dir: self.static_dir.clone(),
            monitors: self.monitors.clone(),
            proxmox_ssh_set: !self.proxmox.ssh.is_empty(),
            proxmox_hostname_prefix: self.proxmox.hostname_prefix.clone(),
            claude: self.claude.clone(),
            clone_groups: self.clone_groups.clone(),
            presets: self.presets.iter().map(Preset::redacted).collect(),
            chroma: self.chroma,
            detector_inference_url: self.detector_inference_url.clone(),
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
    pub monitors: Vec<MonitorSpec>,
    pub proxmox_ssh_set: bool,
    pub proxmox_hostname_prefix: String,
    pub claude: ClaudeConfig,
    pub clone_groups: Vec<CloneGroup>,
    pub presets: Vec<PresetRedacted>,
    pub chroma: ChromaMode,
    pub detector_inference_url: String,
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
        let mons = c.effective_monitors();
        assert_eq!(mons.len(), 2);
        assert_eq!((mons[0].width, mons[0].height, mons[0].x), (2560, 1440, 2560));
        assert!(mons[0].primary);
        assert_eq!(mons[1].x, 0);
        assert!(!mons[1].primary);
    }

    #[test]
    fn chroma_mode_defaults_and_serde() {
        // Default is 4:2:0 (today's behavior / full capacity).
        assert_eq!(ChromaMode::default(), ChromaMode::Yuv420);
        assert_eq!(AppConfig::default().chroma, ChromaMode::Yuv420);
        // Wire/JSON representation is lowercase, matching RMNG_CHROMA values.
        assert_eq!(serde_json::to_string(&ChromaMode::Yuv420).unwrap(), "\"yuv420\"");
        assert_eq!(serde_json::to_string(&ChromaMode::Yuv444).unwrap(), "\"yuv444\"");
        // Missing field falls back to the default (older config.json stays valid).
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.chroma, ChromaMode::Yuv420);
        // Env parser accepts the documented spellings.
        assert_eq!(ChromaMode::from_env_value("yuv444"), Some(ChromaMode::Yuv444));
        assert_eq!(ChromaMode::from_env_value("444"), Some(ChromaMode::Yuv444));
        assert_eq!(ChromaMode::from_env_value(" YUV420 "), Some(ChromaMode::Yuv420));
        assert_eq!(ChromaMode::from_env_value("nonsense"), None);
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
    fn redaction_hides_secrets() {
        let c = AppConfig {
            proxmox: ProxmoxConfig { ssh: "root@10.0.0.100".into(), ..Default::default() },
            presets: vec![
                Preset {
                    name: "med".into(),
                    labels: vec!["Backend".into()],
                    linear_key: "lin_api_secret".into(),
                    vars: vec![EnvVar { key: "A".into(), value: "1".into() }],
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
        assert!(r.proxmox_ssh_set);
    }
}
