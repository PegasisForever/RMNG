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

/// A named monitor-layout preset: a full arrangement the operator can switch to.
/// Distinct from clone-provisioning `Preset` (env/Linear) — this is display geometry only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct LayoutPreset {
    pub name: String,
    pub monitors: Vec<MonitorSpec>,
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

/// One local-forward rule: a TCP port inside this clone (`remote_port`) exposed at
/// `127.0.0.1:<local_port>` on the machine running the native viewer. Persisted in
/// `state.json`; the viewer runs the listener. `id` is derived server-side as
/// `f{local_port}` (local ports are globally unique across all hosts' rules).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct PortForward {
    pub id: String,
    pub remote_port: u16,
    pub local_port: u16,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct Host {
    /// Stable id; equals the Docker container name for cloneable hosts.
    pub id: String,
    /// Endpoint hostname/IP for unmanaged rows. Display-only on managed clones (it
    /// records the container name == `id`; dials resolve via Docker DNS / inspect).
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
    /// True for a managed clone: a Docker container whose *name equals this host's id*
    /// backs it (every Docker call addresses it by that name — no stored container id).
    /// False is a plain unmanaged row (legacy/hand-added, deletable in the UI). Old
    /// `state.json` rows carrying the retired `ctid`/`container` keys load as
    /// unmanaged — serde drops the stale keys.
    #[serde(default)]
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_account_email: Option<String>,
    /// Name of the Claude group this clone is balanced within (sticky — it moves only
    /// when its account exhausts); `None` when bound to a single fixed account. When
    /// set, `claude_account_email` holds the current pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_group: Option<String>,
    /// The operator's Claude *selection* verbatim: `"auto"`, `"none"`, `"group:<name>"`,
    /// or an account email. Distinguishes an auto-managed clone (server picks the best
    /// account and may hot-swap it) from one pinned to a fixed account or opted out of
    /// a token entirely — `claude_account_email` alone can't tell these apart. `None` on
    /// hosts created before this field / when no Claude account is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_selection: Option<String>,
    /// Email of the imported Codex (ChatGPT) account whose token is written into this
    /// clone's `~/.codex/auth.json`. Independent of `claude_account_email` — a clone can
    /// hold both. `None` when no Codex account is assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_account_email: Option<String>,
    /// Name of the Codex group this clone is balanced within (sticky, like `claude_group`);
    /// `None` when bound to a single fixed Codex account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_group: Option<String>,
    /// The operator's Codex *selection* verbatim: `"auto"`, `"none"`, `"group:<name>"`, or
    /// an account email — the Codex twin of `claude_selection`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_selection: Option<String>,
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
    /// True when this clone fell out of `working` (→ idle/offline) since the
    /// operator last viewed it — drives the sidebar "unread" dot. Set by the
    /// monitor poller on that transition, cleared when the clone is activated.
    #[serde(default)]
    pub unread: bool,
    /// Local port-forward rules for this host (see [`PortForward`]). Persisted; the
    /// viewer runs the listeners and reports status out-of-band (volatile `forwards`
    /// SSE event, never stored here).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<PortForward>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum OperationKind {
    Clone,
    Delete,
    /// Pull the clone template from a registry (replaced the retired in-product
    /// `Bootstrap` build). The `bootstrap` alias keeps a persisted legacy op loadable:
    /// `state.rs::read_from_disk` falls back to an EMPTY state on any parse error, so a
    /// stored `"kind":"bootstrap"` op without this alias would wipe every host.
    #[serde(alias = "bootstrap")]
    Pull,
    Commit,
    /// Self-update the control-server: pull a new image + swap the running container.
    Update,
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
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

/// Live per-container resource usage, sampled by the monitor poller each tick and pushed
/// to the frontend as a named `stats` SSE event carrying a `{ hostId: ContainerStats }`
/// map. Deliberately NOT a field of [`ControlState`] / [`Host`]: it changes every tick, so
/// routing it through the state store would rewrite `state.json` every few seconds (every
/// `ControlState` mutation persists — see the control-server's `state.rs`). It rides the
/// same `/events` stream on a separate SSE-only bus instead.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ContainerStats {
    /// CPU use as a percentage of ONE core (100 == a single fully-used core; a container
    /// busy across several cores reads > 100). The frontend divides by 100 to display
    /// "cores".
    pub cpu_pct: f64,
    /// Resident memory in bytes, docker-CLI semantics (`usage` minus reclaimable
    /// `inactive_file` page cache).
    pub mem_used: u64,
    /// Memory limit in bytes; 0 when the daemon reports none.
    pub mem_limit: u64,
}

/// Version + update-available status for the control-server itself, served by
/// `GET /api/server/version`. `current_*` come from the running image's OCI labels /
/// RepoDigest; `remote_digest` from a registry manifest query (no pull). `available` is
/// true when a remote digest was fetched and differs from the running one. `error` carries
/// a non-fatal detail (e.g. registry unreachable) so the UI can show "couldn't check".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct UpdateStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_digest: Option<String>,
    pub available: bool,
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    /// Whether the account can run a clone: true for every imported account of either
    /// provider (the server owns each account's token lifecycle).
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
    /// Name of the active layout preset (mirrored from config so the sidebar switcher
    /// updates live over `/events`). Empty when no presets exist.
    #[serde(default)]
    pub active_layout: String,
    /// Names of all layout presets, in config order — the sidebar's segmented buttons.
    #[serde(default)]
    pub layout_preset_names: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub operations: Vec<Operation>,
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
        assert!(state.claude_accounts.is_empty());
    }

    #[test]
    fn legacy_state_loads_unmanaged() {
        // Old `state.json` shapes: Proxmox-era hosts carry the retired `ctid` key (plus
        // the top-level `templates` list); early docker-port hosts carry the retired
        // `container` id. All are stale and dropped by serde; such hosts load as plain
        // unmanaged rows (`managed: false`).
        let json = r#"{
            "hosts": [
                { "id": "pega-old", "host": "10.0.0.9", "username": "u", "password": "p", "ctid": 5 },
                { "id": "pega-mid", "host": "10.99.0.10", "username": "u", "password": "p",
                  "container": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" }
            ],
            "templates": ["rmng-template"]
        }"#;
        let state: ControlState = serde_json::from_str(json).unwrap();
        assert_eq!(state.hosts.len(), 2);
        assert!(!state.hosts[0].managed);
        assert!(!state.hosts[1].managed);
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
    fn operation_kind_serde_and_bootstrap_alias() {
        // Canonical serialization is the lowercase variant name.
        assert_eq!(serde_json::to_string(&OperationKind::Pull).unwrap(), "\"pull\"");
        assert_eq!(
            serde_json::from_str::<OperationKind>("\"pull\"").unwrap(),
            OperationKind::Pull
        );
        // Legacy persisted ops used `"bootstrap"`; the alias keeps them loadable so a
        // stored op never trips `read_from_disk`'s parse-error → empty-state fallback.
        assert_eq!(
            serde_json::from_str::<OperationKind>("\"bootstrap\"").unwrap(),
            OperationKind::Pull
        );
        // A whole Operation carrying the legacy kind deserializes with everything intact.
        let legacy = r#"{
            "id": "op_1", "kind": "bootstrap", "target": "my-base",
            "status": "running", "step": "queued", "pct": 0.0, "message": "queued",
            "startedAt": 1
        }"#;
        let op: Operation = serde_json::from_str(legacy).unwrap();
        assert_eq!(op.kind, OperationKind::Pull);
        assert_eq!(op.target, "my-base");
    }

    #[test]
    fn host_codex_fields_camelcase() {
        let h = Host {
            id: "h".into(),
            host: "1.2.3.4".into(),
            port: 3389,
            claude_account_email: Some("a@b.c".into()),
            codex_account_email: Some("z@openai.com".into()),
            codex_group: Some("team".into()),
            codex_selection: Some("group:team".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["codexAccountEmail"], "z@openai.com");
        assert_eq!(v["codexGroup"], "team");
        assert_eq!(v["codexSelection"], "group:team");
        // Claude fields still present and untouched.
        assert_eq!(v["claudeAccountEmail"], "a@b.c");
        // Omitted codex fields are not serialized.
        let bare = Host { id: "h2".into(), ..Default::default() };
        let bv = serde_json::to_value(&bare).unwrap();
        assert!(bv.get("codexAccountEmail").is_none());
        // Round-trips.
        let back: Host = serde_json::from_value(v).unwrap();
        assert_eq!(back.codex_selection.as_deref(), Some("group:team"));
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

    #[test]
    fn controlstate_layout_fields_camelcase() {
        let st = ControlState {
            active_layout: "Dual 1440p".into(),
            layout_preset_names: vec!["Dual 1440p".into(), "Single 4K".into()],
            ..Default::default()
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["activeLayout"], "Dual 1440p");
        assert_eq!(v["layoutPresetNames"][1], "Single 4K");
    }

    #[test]
    fn layout_preset_roundtrip_camelcase() {
        let p = LayoutPreset {
            name: "Dual 1440p".into(),
            monitors: vec![MonitorSpec { width: 2560, height: 1440, x: 0, y: 0, primary: true }],
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["name"], "Dual 1440p");
        assert_eq!(v["monitors"][0]["width"], 2560);
        let back: LayoutPreset = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }
}

#[cfg(test)]
mod forward_tests {
    use super::*;

    #[test]
    fn port_forward_round_trips_camel_case() {
        let f = PortForward {
            id: "f8080".into(),
            remote_port: 3000,
            local_port: 8080,
            enabled: true,
            label: Some("dev".into()),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"remotePort\":3000"), "got {json}");
        assert!(json.contains("\"localPort\":8080"), "got {json}");
        assert_eq!(serde_json::from_str::<PortForward>(&json).unwrap(), f);
    }

    #[test]
    fn host_forwards_defaults_empty_and_is_omitted() {
        let json = r#"{"id":"h","host":"h"}"#;
        let h: Host = serde_json::from_str(json).unwrap();
        assert!(h.forwards.is_empty());
        // empty forwards must not serialize (skip_serializing_if)
        assert!(!serde_json::to_string(&h).unwrap().contains("forwards"));
    }
}
