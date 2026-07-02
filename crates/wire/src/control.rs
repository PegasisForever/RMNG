//! Control-plane state — broadcast over `/events` and persisted to `state.json`.
//!
//! The JSON shape is a **byte-for-byte superset** of the current
//! `control-server/app/lib/types.ts` so the React frontend (and, during cutover,
//! the legacy Rust client) keep parsing it unchanged. Note `Host` mixes casing:
//! the fields inherited from the legacy control server stay snake_case
//! (`gdm_username`) while the server-only extras are camelCase (`claudeAccountEmail`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

fn default_rdp_port() -> u16 {
    3389
}

/// One monitor in the global desired layout: size, position (top-left in the unified
/// desktop, pixels) and whether it's primary. `x`/`y`/`primary` default for back-compat
/// with size-only configs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct MonitorSpec {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub x: u32,
    #[serde(default)]
    pub y: u32,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum Provider {
    Claude,
    Codex,
}

/// The agent's last self-reported verdict (via the `set_state` MCP tool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum AgentReport {
    Working,
    Idle,
}

/// Effective host state for the UI, derived by the server-side poller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum MonitorState {
    Working,
    Idle,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct Host {
    /// Stable id; equals the Proxmox container hostname for cloneable hosts.
    pub id: String,
    /// RDP/media server hostname or IP.
    pub host: String,
    /// Port (defaults to 3389 for the legacy RDP path).
    #[serde(default = "default_rdp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    // The two GDM fields keep snake_case JSON (legacy field names). ts-rs
    // can't parse the combined serde attr, so pin the TS name explicitly too.
    #[serde(
        default,
        rename = "gdm_username",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(rename = "gdm_username")]
    pub gdm_username: Option<String>,
    #[serde(
        default,
        rename = "gdm_password",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(rename = "gdm_password")]
    pub gdm_password: Option<String>,

    // --- server-only extras (camelCase) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_account_email: Option<String>,
    /// Name of the Claude group this clone rotates within; `None` when bound to a
    /// single fixed account. When set, `claude_account_email` holds the current pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_group: Option<String>,
    /// The operator's Claude *selection* verbatim: `"auto"`, `"none"`, `"group:<name>"`,
    /// or an account email. Distinguishes an auto-managed clone (server picks the best
    /// account and may hot-swap it) from one pinned to a fixed account or opted out of
    /// a token entirely — `claude_account_email` alone can't tell these apart. `None` on
    /// hosts created before this field / when no Claude account is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_selection: Option<String>,
    /// Lowercase Linear workspace name / ticket prefix (e.g. `"we"`). An open
    /// string: the workspace set is config (Settings → Linear API keys), not an enum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_ticket_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_report: Option<AgentReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_state: Option<MonitorState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum OperationKind {
    Clone,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum OperationStatus {
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct Operation {
    pub id: String,
    pub kind: OperationKind,
    /// Host id being created (clone) or removed (delete).
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub status: OperationStatus,
    /// Current step key (maps to a coarse percentage in the UI).
    pub step: String,
    /// 0–100.
    pub pct: f64,
    pub message: String,
    /// Rolling log lines for the operation.
    #[serde(default)]
    pub log: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctid: Option<u32>,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

/// 0–100 utilization for a rolling usage window + when it resets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ClaudeUsageWindow {
    pub pct: f64,
    /// ISO timestamp when the window resets, or null if unknown.
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ClaudeSpend {
    pub used_cents: i64,
    pub limit_cents: Option<i64>,
    pub pct: f64,
    pub currency: String,
    pub resets_at: Option<String>,
}

/// A non-secret per-account usage view (tokens never enter this struct).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ClaudeUsage {
    /// Stable id: claude `${email}|${orgUuid}`, codex `codex:<id>`.
    pub id: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    pub last_updated: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<ClaudeUsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<ClaudeUsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<ClaudeSpend>,
}

/// The top-level state broadcast over `/events` and persisted to `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ControlState {
    /// Id of the host that should be displayed. May be absent or point at a host
    /// not in the list; consumers must tolerate both.
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub monitors: Vec<MonitorSpec>,
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub operations: Vec<Operation>,
    /// Host ids that are templates: not deletable; cloned instead.
    #[serde(default)]
    pub templates: Vec<String>,
    /// Per-Claude-account usage view (no tokens).
    #[serde(default)]
    pub claude_accounts: Vec<ClaudeUsage>,
}

impl ControlState {
    /// The currently selected host, if it exists in the list.
    pub fn selected_host(&self) -> Option<&Host> {
        let sel = self.selected.as_deref()?;
        self.hosts.iter().find(|h| h.id == sel)
    }
}

// --- per-host chat (stored at data/chats/<id>.json, not in ControlState) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ChatMessage {
    pub id: String,
    pub role: ChatRole,
    pub text: String,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct Chat {
    /// Reserved; always null on new writes (agent-wrapper owns session continuity).
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_shared_example() {
        // The exact shape the legacy `ControlState` test used.
        let json = r#"{
            "selected": "host-a",
            "hosts": [
                { "id": "host-a", "host": "10.0.0.5", "username": "user", "password": "pw" },
                { "id": "host-b", "host": "10.0.0.6", "port": 3390, "username": "user", "password": "pw" }
            ]
        }"#;
        let state: ControlState = serde_json::from_str(json).unwrap();
        assert_eq!(state.hosts.len(), 2);
        assert_eq!(state.selected.as_deref(), Some("host-a"));
        assert_eq!(state.selected_host().unwrap().host, "10.0.0.5");
        assert_eq!(state.hosts[0].port, 3389); // default
        assert_eq!(state.hosts[1].port, 3390);
        assert!(state.operations.is_empty());
        assert!(state.templates.is_empty());
        assert!(state.claude_accounts.is_empty());
    }

    #[test]
    fn host_casing_matches_typescript() {
        // gdm_* stay snake_case; extras are camelCase.
        let h = Host {
            id: "h".into(),
            host: "1.2.3.4".into(),
            port: 3389,
            gdm_username: Some("u".into()),
            claude_account_email: Some("a@b.c".into()),
            linear_workspace: Some("we".into()),
            monitor_state: Some(MonitorState::Working),
            ..Default::default()
        };
        let v = serde_json::to_value(&h).unwrap();
        assert!(v.get("gdm_username").is_some(), "gdm_username stays snake_case");
        assert_eq!(v["claudeAccountEmail"], "a@b.c");
        assert_eq!(v["linearWorkspace"], "we");
        assert_eq!(v["monitorState"], "working");
        // omitted optionals are not serialized
        assert!(v.get("source").is_none());
    }

    #[test]
    fn ts_binding_keeps_gdm_snake_case() {
        // Guards the ts-rs quirk: the gdm_* fields must stay snake_case in the
        // generated TS so the frontend reads the same keys the server emits.
        let d = <Host as ts_rs::TS>::decl();
        assert!(d.contains("gdm_username"), "binding lost gdm_username: {d}");
        assert!(!d.contains("gdmUsername"), "binding camelCased gdm_username: {d}");
    }

    #[test]
    fn controlstate_roundtrip_camelcase() {
        let st = ControlState {
            selected: Some("h".into()),
            monitors: vec![MonitorSpec { width: 1920, height: 1080, x: 0, y: 0, primary: true }],
            claude_accounts: vec![ClaudeUsage {
                id: "a@b|org".into(),
                email: "a@b".into(),
                provider: Some(Provider::Claude),
                active: true,
                assignable: Some(true),
                error: None,
                stale: None,
                last_updated: 123,
                five_hour: Some(ClaudeUsageWindow { pct: 12.5, resets_at: None }),
                seven_day: None,
                spend: None,
            }],
            ..Default::default()
        };
        let s = serde_json::to_string(&st).unwrap();
        assert!(s.contains("\"claudeAccounts\""));
        assert!(s.contains("\"fiveHour\""));
        let back: ControlState = serde_json::from_str(&s).unwrap();
        assert_eq!(st, back);
    }
}
