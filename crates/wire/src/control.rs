//! Control-plane state — broadcast over `/events` and persisted to `state.json`.
//!
//! The JSON shape is a **byte-for-byte superset** of the current
//! `control-server/app/lib/types.ts` so the React frontend (and, during cutover,
//! the legacy Rust client) keep parsing it unchanged. Note the `Clone` type mixes casing:
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

/// One column of the dashboard board, holding clone ids top to bottom.
///
/// A clone the operator has not filed anywhere is not stored: the board draws it in the
/// first column, which is how a newly created clone shows up without anyone writing it here
/// first. Ids of clones that no longer exist are harmless — the board ignores them, and the
/// next move rewrites the column without them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct BoardColumn {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub clone_ids: Vec<String>,
    /// Dropping a clone here archives it, and dragging one out starts it again.
    ///
    /// This is a rule about the *gesture*, not a filter on the contents: a clone archived
    /// from its own menu stays in whatever column it was already in. Nothing enforces that
    /// the two agree, which is what lets the card sit still while the server works — the
    /// column is where the operator put it, and `archived` is what the server made of it.
    #[serde(default)]
    pub archive: bool,
}

/// Server-owned lifecycle state. Docker supplies container liveness while passive proxy token
/// activity distinguishes `working` from a running-but-not-working (`idle`) clone.
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
/// `f{local_port}` (local ports are globally unique across all clones' rules).
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
// The Rust type is `RmngClone` (a bare `Clone` would shadow `std::clone::Clone`); the
// exported TypeScript type and every user-facing reference is a plain `Clone`.
#[ts(export, rename = "Clone", export_to = "../../../frontend/app/lib/wire/")]
pub struct RmngClone {
    /// Stable id; equals the Docker container name for a managed clone.
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
    /// True for a managed clone: a Docker container whose *name equals this clone's id*
    /// backs it (every Docker call addresses it by that name — no stored container id).
    /// False is a plain unmanaged row (legacy/hand-added, deletable in the UI). Old
    /// `state.json` rows carrying the retired `ctid`/`container` keys load as
    /// unmanaged — serde drops the stale keys.
    #[serde(default)]
    pub managed: bool,
    /// True when this managed clone is intentionally stopped but retained. Its container,
    /// named volumes, notes, and chat history remain available for a later unarchive.
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Email of the imported Claude account whose access token is written into this clone's
    /// `~/.claude/.credentials.json`. The control-server owns the refresh lifecycle and
    /// re-pushes on every rotation; the clone never holds a refresh token.
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
    /// clones created before this field / when no Claude account is configured.
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
    /// Clone preset name used at creation. New control-server versions persist this so
    /// reconciliation can rebuild `/etc/environment` without relying on a guest-side
    /// legacy env file. Older clones may not have it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_label: Option<String>,
    /// Current server-owned lifecycle state. It is derived from Docker liveness and passive
    /// proxy token activity, never reported by a clone-local process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_state: Option<MonitorState>,
    /// The clone container's IPv4 on the rmng bridge network — the address other
    /// clones can dial it at directly (alongside its `id`, which Docker's embedded
    /// DNS resolves to the same clone). Populated by the monitor poller from a Docker
    /// inspect each tick; `None` for unmanaged rows or a stopped/detached container.
    /// A recreated container's new IP self-heals on the next poll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_ip: Option<String>,
    /// Set when a clone transitions from `working` to `idle`/`offline` while it is not selected.
    /// Clearing the selection state is an explicit operator action, not a clone report.
    #[serde(default)]
    pub unread: bool,
    /// A headless clone has **no desktop**: its display (`gnome-headless.service`) and capture
    /// daemon (`rmng-clone-daemon.service`) are disabled at create time, so it streams no video.
    /// Selecting it drives the viewer's tmux tab view instead of an H.264 desktop stream (see
    /// the control-server `termplane`). Created from the same template as a regular clone.
    #[serde(default)]
    pub headless: bool,
    /// The id of this clone's parent, when it is a sub clone. One level deep only — a clone
    /// that has a parent is never itself a parent. `None` = top-level. Purely cosmetic
    /// grouping in the sidebar and `rmng clone ls`; a sub clone is otherwise an ordinary managed
    /// clone (its own group binding, router key, tokens, and video). Cascade-deleted with
    /// its parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Local port-forward rules for this clone (see [`PortForward`]). Persisted; the
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
    Archive,
    Unarchive,
    /// Pull the clone template from a registry (replaced the retired in-product
    /// `Bootstrap` build). The `bootstrap` alias keeps a persisted legacy op loadable:
    /// `state.rs::read_from_disk` falls back to an EMPTY state on any parse error, so a
    /// stored `"kind":"bootstrap"` op without this alias would wipe every clone.
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
    /// Clone id being created, archived, restored, or removed; image reference for template jobs.
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
/// map. Deliberately NOT a field of [`ControlState`] / [`RmngClone`]: it changes every tick, so
/// routing it through the state store would rewrite `state.json` every few seconds (every
/// `ControlState` mutation persists — see the control-server's `state.rs`). It rides the
/// same `/events` stream on a separate SSE-only bus instead.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ContainerStats {
    /// CPU use as a percentage of total host capacity (100 == every available core busy).
    pub cpu_pct: f64,
    /// RAM usage excluding reclaimable page cache, plus swap usage, in bytes. Tmpfs and
    /// shared-memory charges remain included.
    pub mem_used: u64,
    /// RAM plus swap limit in bytes; 0 when either cgroup limit is unbounded or unavailable.
    pub mem_limit: u64,
}

/// Live resource usage for the entire CT 105 LXC that hosts RMNG. Published as the volatile
/// `lxcStats` SSE event, separately from per-clone [`ContainerStats`] rows, so control-server,
/// Docker, registry, cache, and unmanaged CT work are included without entering `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct LxcStats {
    /// CT-wide CPU use; 100 means CT 105's enforced 16-CPU capacity was busy. `None` is
    /// emitted until two cgroup samples establish a rate.
    pub cpu_pct: Option<f64>,
    /// RAM usage excluding reclaimable page cache, plus swap usage, in bytes. Tmpfs and
    /// shared-memory charges remain included.
    pub mem_used: u64,
    /// RAM plus swap limit in bytes; 0 when either cgroup limit is unbounded or unavailable.
    pub mem_limit: u64,
    /// Physical, compression-aware use of CT 105's ZFS root filesystem, in bytes.
    pub disk_used: Option<u64>,
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
    /// Stable, unique id: `${group}|${provider}|${email}` (group- and provider-scoped, since
    /// one email can be authenticated into several groups and, within a group, under more than
    /// one provider). Used only as an opaque key — never parsed positionally.
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
    /// Claude only: the model-scoped weekly limit for the Fable model family. Purely
    /// informational — it never gates account rotation (see the rotator, which keys off
    /// `five_hour`/`seven_day` only). `None` for Codex and when the account has no such
    /// scoped limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fable: Option<ClaudeUsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<ClaudeSpend>,
    /// Codex only: banked rate-limit reset credits ("usage resets") left on the
    /// account. `None` for Claude (no such concept) and when usage is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<i64>,
}

/// One recorded auto-consumed (or reserved) Codex reset. Persisted in `ControlState`
/// so a server restart can't re-spend on an account already reset this 7d window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct CodexResetMark {
    pub account_id: String,
    /// The 7d window (its `resets_at` epoch **seconds**) this reset was spent against —
    /// the cooldown key. An account is on cooldown while its current 7d window matches.
    pub window_resets_at: i64,
    /// Wall-clock ms when the mark was reserved / consume attempted (audit / UI tooltip).
    pub consumed_at: i64,
    /// Idempotency key sent to `/consume` for this reservation (audit; enables a future
    /// safe same-key retry — v1 does not retry within a window).
    pub redeem_request_id: String,
}

/// All-time token totals for one clone, summed across both providers.
///
/// Accumulated from the agent CLIs' own session logs — the JSONL transcripts under
/// `~/.claude/projects` and `~/.codex/sessions` — which every agent writes regardless of
/// whether RMNG or a human at a shell launched it. Cumulative
/// since the clone was first scanned and persisted in `state.json`, because both CLIs prune
/// old session files — a figure re-derived from whatever logs still exist would silently
/// shrink as history ages out.
///
/// Counts the *clone*, not the account: hot-swapping a clone's account leaves the total
/// climbing across the swap. It answers "which clone is expensive", not "what did this
/// account spend" — the per-account 5h/7d windows in [`ClaudeUsage`] answer the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct CloneTokens {
    /// Newly processed input tokens. Cache *reads* are excluded — one sampled response had
    /// 210,735 cache-read tokens against 1 real input token, so including them would swamp
    /// the figure. Cache *creation* is included: those tokens really were processed.
    pub input_tokens: u64,
    /// Newly generated output tokens, including Codex reasoning tokens (its client-facing
    /// `output_tokens` does not already include them).
    pub output_tokens: u64,
    /// True when this clone's most recent Claude response came from the Fable model family
    /// within the last few minutes. Derived server-side from a private timestamp (never
    /// sent) and re-projected each scan so it decays back to false; drives the sidebar
    /// badge beside the Claude account.
    #[serde(default)]
    pub fable_active: bool,
}

/// The top-level state broadcast over `/events` and persisted to `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ControlState {
    /// Id of the clone that should be displayed. May be absent or point at a clone
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
    pub hosts: Vec<RmngClone>,
    /// The board's columns, left to right. Empty until the operator makes one, in which
    /// case the frontend draws a single default column so no clone is ever hidden.
    #[serde(default)]
    pub board_columns: Vec<BoardColumn>,
    /// The operator's own arrangement of the ticket column, top to bottom. Linear owns which
    /// issues exist. Their arrangement is the one thing about them rmng owns, which is why
    /// it is the one thing about them that is stored here.
    ///
    /// Ids are stored lowercased, because that is how the browser compares them when it
    /// applies the order. An id for a ticket that no longer exists is ignored rather than
    /// pruned: a closed ticket costs one stale string, and no consumer has to run a cleanup
    /// pass to keep this honest.
    #[serde(default)]
    pub ticket_order: Vec<String>,
    #[serde(default)]
    pub operations: Vec<Operation>,
    /// Per-account usage view (no tokens). Despite the name it holds **both** providers'
    /// rows, distinguished by [`ClaudeUsage::provider`]; `clone_ops::replace_provider_views`
    /// is what lets the Claude and Codex pollers publish here without clobbering each other.
    #[serde(default)]
    pub claude_accounts: Vec<ClaudeUsage>,
    /// Codex auto-reset bookkeeping (cooldown). Non-secret; changes at most once per
    /// account per week, so it belongs in `state.json` (unlike per-tick stats).
    #[serde(default)]
    pub codex_reset_marks: Vec<CodexResetMark>,
    /// All-time per-clone token totals, keyed by clone id. Unlike the volatile CPU/RAM
    /// stats map (which rides its own SSE bus precisely because it changes every tick),
    /// this is persisted: it is cumulative, and the logs it is derived from get pruned.
    ///
    /// **`BTreeMap`, not `HashMap`, and that is load-bearing.** This is the only map in
    /// `ControlState`, and `state.rs`'s file watcher decides whether an on-disk change came
    /// from outside by *string-comparing* a reserialization against what it last wrote. A
    /// `HashMap` reserializes in a different key order after a round-trip through the parser,
    /// so with two or more entries that compare never matches: every one of our own writes
    /// would look like a hand-edit, triggering a redundant full-state SSE broadcast and a
    /// racy out-of-band state replacement. `BTreeMap`'s ordered output keeps the gate honest.
    /// The JSON shape is identical either way, so no consumer can tell the difference.
    #[serde(default)]
    pub clone_tokens: std::collections::BTreeMap<String, CloneTokens>,
}

impl ControlState {
    /// The currently selected clone, if it exists in the list.
    pub fn selected_clone(&self) -> Option<&RmngClone> {
        let sel = self.selected.as_deref()?;
        self.hosts.iter().find(|h| h.id == sel)
    }
}

// --- per-clone chat (stored at data/chats/<id>.json, not in ControlState) ---

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

/// A user message the operator queued for later delivery to a clone's agent.
///
/// Stored apart from the `Chat` itself (`data/schedules/<id>.json`): a scheduled message
/// is not part of the conversation until it actually fires, at which point it enters the
/// chat as an ordinary `ChatMessage` and disappears from here. Keeping the two files
/// separate means a delivery never has to rewrite the transcript and vice-versa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ScheduledMessage {
    pub id: String,
    pub text: String,
    /// When to deliver it, epoch milliseconds (UTC). The UI collects a *local* wall-clock
    /// time and converts; the server only ever compares absolute instants, so a client in
    /// another timezone (or a DST shift between now and then) can't move the delivery.
    pub at: i64,
    /// When the operator queued it, epoch milliseconds. Purely informational.
    pub created_at: i64,
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
        assert_eq!(state.selected_clone().unwrap().host, "10.0.0.5");
        assert_eq!(state.hosts[0].port, 3389); // default
        assert_eq!(state.hosts[1].port, 3390);
        assert!(state.operations.is_empty());
        assert!(state.claude_accounts.is_empty());
    }

    #[test]
    fn legacy_state_loads_unmanaged() {
        // Old `state.json` shapes: Proxmox-era clones carry the retired `ctid` key (plus
        // the top-level `templates` list); early docker-port clones carry the retired
        // `container` id. All are stale and dropped by serde; such clones load as plain
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
    fn clone_casing_matches_typescript() {
        // gdm_* stay snake_case; extras are camelCase.
        let h = RmngClone {
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
        assert!(
            v.get("gdm_username").is_some(),
            "gdm_username stays snake_case"
        );
        assert_eq!(v["claudeAccountEmail"], "a@b.c");
        assert_eq!(v["linearWorkspace"], "we");
        assert_eq!(v["monitorState"], "working");
        assert_eq!(v["archived"], false);
        // omitted optionals are not serialized
        assert!(v.get("source").is_none());
    }

    #[test]
    fn ts_binding_keeps_gdm_snake_case() {
        // Guards the ts-rs quirk: the gdm_* fields must stay snake_case in the
        // generated TS so the frontend reads the same keys the server emits.
        let d = <RmngClone as ts_rs::TS>::decl();
        assert!(d.contains("gdm_username"), "binding lost gdm_username: {d}");
        assert!(
            !d.contains("gdmUsername"),
            "binding camelCased gdm_username: {d}"
        );
    }

    #[test]
    fn operation_kind_serde_and_bootstrap_alias() {
        // Canonical serialization is the lowercase variant name.
        assert_eq!(
            serde_json::to_string(&OperationKind::Pull).unwrap(),
            "\"pull\""
        );
        assert_eq!(
            serde_json::to_string(&OperationKind::Archive).unwrap(),
            "\"archive\""
        );
        assert_eq!(
            serde_json::to_string(&OperationKind::Unarchive).unwrap(),
            "\"unarchive\""
        );
        assert_eq!(
            serde_json::from_str::<OperationKind>("\"pull\"").unwrap(),
            OperationKind::Pull
        );
        assert_eq!(
            serde_json::from_str::<OperationKind>("\"archive\"").unwrap(),
            OperationKind::Archive
        );
        assert_eq!(
            serde_json::from_str::<OperationKind>("\"unarchive\"").unwrap(),
            OperationKind::Unarchive
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
    fn legacy_clone_defaults_to_active_and_archive_state_roundtrips() {
        let legacy: RmngClone = serde_json::from_str(r#"{ "id": "h", "host": "h" }"#).unwrap();
        assert!(!legacy.archived);

        let archived = RmngClone {
            id: "h".into(),
            host: "h".into(),
            managed: true,
            archived: true,
            ..Default::default()
        };
        let value = serde_json::to_value(&archived).unwrap();
        assert_eq!(value["archived"], true);
        assert!(serde_json::from_value::<RmngClone>(value).unwrap().archived);
    }

    #[test]
    fn clone_codex_fields_camelcase() {
        let h = RmngClone {
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
        // Omitted account fields are not serialized — the six are independent options, so a
        // clone with no accounts assigned carries none of the keys at all.
        let bare = RmngClone {
            id: "h2".into(),
            ..Default::default()
        };
        let bv = serde_json::to_value(&bare).unwrap();
        assert!(bv.get("codexAccountEmail").is_none());
        assert!(bv.get("claudeAccountEmail").is_none());
        // A `state.json` row written under the group-proxy model carries a `group` key that
        // no longer exists; serde drops it rather than failing the whole file.
        let old: RmngClone =
            serde_json::from_str(r#"{"id":"h3","host":"h3","group":"team"}"#).unwrap();
        assert_eq!(old.id, "h3");
        assert!(old.claude_account_email.is_none());
        // Round-trips.
        let back: RmngClone = serde_json::from_value(v).unwrap();
        assert_eq!(back.codex_selection.as_deref(), Some("group:team"));
    }

    #[test]
    fn controlstate_roundtrip_camelcase() {
        let st = ControlState {
            selected: Some("h".into()),
            monitors: vec![MonitorSpec {
                width: 1920,
                height: 1080,
                x: 0,
                y: 0,
                primary: true,
            }],
            claude_accounts: vec![ClaudeUsage {
                id: "a@b|org".into(),
                email: "a@b".into(),
                provider: Some(Provider::Claude),
                active: true,
                assignable: Some(true),
                error: None,
                stale: None,
                last_updated: 123,
                five_hour: Some(ClaudeUsageWindow {
                    pct: 12.5,
                    resets_at: None,
                }),
                seven_day: None,
                fable: None,
                spend: None,
                reset_credits: Some(3),
            }],
            ..Default::default()
        };
        let s = serde_json::to_string(&st).unwrap();
        assert!(s.contains("\"claudeAccounts\""));
        assert!(s.contains("\"fiveHour\""));
        assert!(s.contains("\"resetCredits\":3"));
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
            monitors: vec![MonitorSpec {
                width: 2560,
                height: 1440,
                x: 0,
                y: 0,
                primary: true,
            }],
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["name"], "Dual 1440p");
        assert_eq!(v["monitors"][0]["width"], 2560);
        let back: LayoutPreset = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn codex_reset_marks_roundtrip_camelcase() {
        let st = ControlState {
            codex_reset_marks: vec![CodexResetMark {
                account_id: "codex:acc-1".into(),
                window_resets_at: 1783392770,
                consumed_at: 1783168000000,
                redeem_request_id: "abc123".into(),
            }],
            ..Default::default()
        };
        let s = serde_json::to_string(&st).unwrap();
        assert!(s.contains("\"codexResetMarks\""));
        assert!(s.contains("\"windowResetsAt\":1783392770"));
        assert!(s.contains("\"redeemRequestId\":\"abc123\""));
        let back: ControlState = serde_json::from_str(&s).unwrap();
        assert_eq!(st, back);
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
    fn clone_forwards_defaults_empty_and_is_omitted() {
        let json = r#"{"id":"h","host":"h"}"#;
        let h: RmngClone = serde_json::from_str(json).unwrap();
        assert!(h.forwards.is_empty());
        // empty forwards must not serialize (skip_serializing_if)
        assert!(!serde_json::to_string(&h).unwrap().contains("forwards"));
    }
}
