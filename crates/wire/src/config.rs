//! `AppConfig` — every setting, edited via the Settings UI (no hand-edited files).
//!
//! Secrets (proxmox ssh target, Linear keys, clone-account tokens) live only in
//! the server's `config.json` (0600) and are **never** placed in `ControlState`
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

/// A named set of environment variables, applied to a clone's session when chosen at
/// clone time (written to `~/.config/environment.d/30-rmng-preset.conf`). Vars that must
/// ALWAYS be present (e.g. `XDG_CURRENT_DESKTOP`) are NOT presets — they're baked into the
/// template's base session env by `provision-clone.sh`, inherited by every clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct EnvPreset {
    pub name: String,
    #[serde(default)]
    pub vars: Vec<EnvVar>,
}

/// A Claude account credential pair (both fields secret). The refresh token (+ a
/// cached short-lived access token) is used **only** to read usage; the long-lived
/// token is installed into a clone's `~/.claude/.credentials.json` to run Claude Code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CloneAccount {
    pub email: String,
    #[serde(default)]
    pub long_lived_token: String,
    #[serde(default)]
    pub refresh_token: String,
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

/// One Linear workspace: `name` is the lowercase ticket prefix, which doubles as
/// the Linear team key (`we` ↔ `WE-142` tickets); `key` is the personal API key
/// (**secret** — never sent to the browser).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LinearKey {
    pub name: String,
    #[serde(default)]
    pub key: String,
}

/// The Linear workspaces — an editable list (Settings → "Linear API keys").
/// Serialized as a plain array; old `config.json` files with the fixed
/// `{we,dev,hh,per}` object still parse (see [`LinearConfigCompat`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(from = "LinearConfigCompat")]
pub struct LinearConfig(pub Vec<LinearKey>);

impl LinearConfig {
    /// The API key for a workspace name (case-insensitive), if set non-empty.
    pub fn key_for(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|k| k.name.eq_ignore_ascii_case(name))
            .map(|k| k.key.as_str())
            .filter(|k| !k.is_empty())
    }

    /// Configured workspace names, for pickers and error messages.
    pub fn names(&self) -> Vec<&str> {
        self.0.iter().map(|k| k.name.as_str()).collect()
    }
}

/// Both accepted `linear` wire shapes: the current list, and the legacy fixed
/// four-workspace object written by pre-list versions.
#[derive(Deserialize)]
#[serde(untagged)]
enum LinearConfigCompat {
    List(Vec<LinearKey>),
    Legacy {
        #[serde(default)]
        we: Option<String>,
        #[serde(default)]
        dev: Option<String>,
        #[serde(default)]
        hh: Option<String>,
        #[serde(default)]
        per: Option<String>,
    },
}

impl From<LinearConfigCompat> for LinearConfig {
    fn from(c: LinearConfigCompat) -> Self {
        match c {
            LinearConfigCompat::List(keys) => Self(keys),
            LinearConfigCompat::Legacy { we, dev, hh, per } => Self(
                [("we", we), ("dev", dev), ("hh", hh), ("per", per)]
                    .into_iter()
                    .filter_map(|(name, key)| {
                        let key = key.unwrap_or_default();
                        (!key.is_empty()).then(|| LinearKey { name: name.into(), key })
                    })
                    .collect(),
            ),
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
    pub linear: LinearConfig,
    #[serde(default)]
    pub claude: ClaudeConfig,
    #[serde(default)]
    pub clone_accounts: Vec<CloneAccount>,
    /// Named account pools a clone can be bound to for rotation (members are
    /// `clone_accounts` emails).
    #[serde(default)]
    pub clone_groups: Vec<CloneGroup>,
    /// Named environment-variable presets the operator picks from at clone time.
    #[serde(default)]
    pub env_presets: Vec<EnvPreset>,
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
            linear: LinearConfig::default(),
            claude: ClaudeConfig::default(),
            clone_accounts: Vec::new(),
            clone_groups: Vec::new(),
            env_presets: Vec::new(),
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
            linear_keys: self
                .linear
                .0
                .iter()
                .map(|k| LinearKeyRedacted { name: k.name.clone(), set: !k.key.is_empty() })
                .collect(),
            claude: self.claude.clone(),
            clone_accounts: self
                .clone_accounts
                .iter()
                .map(|a| CloneAccountRedacted {
                    email: a.email.clone(),
                    long_lived_token_set: !a.long_lived_token.is_empty(),
                    refresh_token_set: !a.refresh_token.is_empty(),
                })
                .collect(),
            clone_groups: self.clone_groups.clone(),
            env_presets: self.env_presets.clone(),
            chroma: self.chroma,
            detector_inference_url: self.detector_inference_url.clone(),
        }
    }
}

/// A Linear workspace as shown to the browser: its name + whether a key is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct LinearKeyRedacted {
    pub name: String,
    pub set: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct CloneAccountRedacted {
    pub email: String,
    pub long_lived_token_set: bool,
    pub refresh_token_set: bool,
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
    pub linear_keys: Vec<LinearKeyRedacted>,
    pub claude: ClaudeConfig,
    pub clone_accounts: Vec<CloneAccountRedacted>,
    pub clone_groups: Vec<CloneGroup>,
    pub env_presets: Vec<EnvPreset>,
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
    fn linear_config_parses_both_shapes() {
        // Current shape: a plain list.
        let c: AppConfig =
            serde_json::from_str(r#"{ "linear": [{ "name": "we", "key": "K1" }, { "name": "ops", "key": "K2" }] }"#)
                .unwrap();
        assert_eq!(c.linear.names(), vec!["we", "ops"]);
        assert_eq!(c.linear.key_for("OPS"), Some("K2")); // case-insensitive
        // Legacy shape: the fixed four-workspace object (null/missing keys dropped).
        let c: AppConfig =
            serde_json::from_str(r#"{ "linear": { "we": "K1", "hh": null, "per": "K3" } }"#).unwrap();
        assert_eq!(c.linear.names(), vec!["we", "per"]);
        assert_eq!(c.linear.key_for("we"), Some("K1"));
        assert_eq!(c.linear.key_for("hh"), None);
        // Round-trips as the list shape.
        let v = serde_json::to_value(&c.linear).unwrap();
        assert!(v.is_array());
        // Missing field → empty list.
        let c: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(c.linear.0.is_empty());
    }

    #[test]
    fn redaction_hides_secrets() {
        let c = AppConfig {
            proxmox: ProxmoxConfig { ssh: "root@10.0.0.100".into(), ..Default::default() },
            linear: LinearConfig(vec![
                LinearKey { name: "we".into(), key: "lin_api_secret".into() },
                LinearKey { name: "ops".into(), key: String::new() },
            ]),
            clone_accounts: vec![CloneAccount {
                email: "a@b".into(),
                long_lived_token: "sk-ant-oat01-x".into(),
                refresh_token: "".into(),
            }],
            ..Default::default()
        };
        let r = c.redacted();
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("sk-ant-oat01-x"));
        assert!(!json.contains("10.0.0.100"));
        assert!(!json.contains("lin_api_secret"));
        assert_eq!(r.linear_keys.len(), 2);
        assert!(r.linear_keys[0].set && r.linear_keys[0].name == "we");
        assert!(!r.linear_keys[1].set);
        assert!(r.proxmox_ssh_set);
        assert!(r.clone_accounts[0].long_lived_token_set);
        assert!(!r.clone_accounts[0].refresh_token_set);
    }
}
