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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LinearConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub we: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per: Option<String>,
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
    /// Host id whose mounted home holds claude-swap's data dir (the template).
    #[serde(default = "default_template_host_id")]
    pub template_host_id: String,
    /// claude-swap data dir relative to the host's home.
    #[serde(default = "default_swap_subpath")]
    pub swap_data_subpath: String,
    /// Hot-swap a clone to another account when its usage is exhausted.
    #[serde(default)]
    pub auto_swap_on_exhaustion: bool,
}

fn default_template_host_id() -> String {
    "pega-dev-template".into()
}
fn default_swap_subpath() -> String {
    ".local/share/claude-swap".into()
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            poll_secs: 600,
            pinned_email: None,
            template_host_id: default_template_host_id(),
            swap_data_subpath: default_swap_subpath(),
            auto_swap_on_exhaustion: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct TemplateConfig {
    pub base_image: String,
    pub cores: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,
}

impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            base_image: "local:vztmpl/ubuntu-26.04-standard_26.04-1_amd64.tar.zst".into(),
            cores: 4,
            memory_mb: 8192,
            disk_gb: 40,
        }
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
    #[serde(default)]
    pub template: TemplateConfig,
    /// Named environment-variable presets the operator picks from at clone time.
    #[serde(default)]
    pub env_presets: Vec<EnvPreset>,
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
            template: TemplateConfig::default(),
            env_presets: Vec::new(),
        }
    }
}

fn default_agent_port() -> u16 {
    4096
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
            linear_keys_set: LinearKeysSet {
                we: self.linear.we.is_some(),
                dev: self.linear.dev.is_some(),
                hh: self.linear.hh.is_some(),
                per: self.linear.per.is_some(),
            },
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
            template: self.template.clone(),
            env_presets: self.env_presets.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct LinearKeysSet {
    pub we: bool,
    pub dev: bool,
    pub hh: bool,
    pub per: bool,
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
    pub linear_keys_set: LinearKeysSet,
    pub claude: ClaudeConfig,
    pub clone_accounts: Vec<CloneAccountRedacted>,
    pub template: TemplateConfig,
    pub env_presets: Vec<EnvPreset>,
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
    fn redaction_hides_secrets() {
        let c = AppConfig {
            proxmox: ProxmoxConfig { ssh: "root@10.0.0.100".into(), ..Default::default() },
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
        assert!(r.proxmox_ssh_set);
        assert!(r.clone_accounts[0].long_lived_token_set);
        assert!(!r.clone_accounts[0].refresh_token_set);
    }
}
