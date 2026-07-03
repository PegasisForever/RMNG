// Shared types for the control plane.
//
// `ControlState` is a superset of the legacy Rust `ControlState`
// (`{selected, hosts, monitors}`). The native RDP client deserializes the SSE
// payload as that Rust struct, and serde ignores unknown fields — so the extra
// `operations` field (and the per-host `container`/`source` extras) ride along
// invisibly. In-progress clones are kept OUT of `hosts` until they are fully
// provisioned, so the client never tries to connect to a half-built container.

export interface MonitorSpec {
  width: number;
  height: number;
}

export interface Host {
  /** Stable id; equals the Docker container name for cloneable hosts. */
  id: string;
  /** RDP server hostname or IP. */
  host: string;
  /** RDP server port (defaults to 3389). */
  port: number;
  username: string;
  password: string;
  domain?: string;
  gdm_username?: string;
  gdm_password?: string;

  // --- server-only extras (ignored by the Rust client) ---
  /**
   * True for a managed clone: a Docker container whose *name equals this host's
   * id* backs it (no container id is stored anywhere). False/absent is a plain
   * unmanaged row (deletable in the UI). Old `state.json` rows carrying the
   * retired `ctid`/`container` keys load unmanaged — serde drops the stale keys.
   */
  managed?: boolean;
  /** The clone-source image reference this host was cloned from (`rmng/template:<name>`). */
  source?: string;
  /**
   * Email of the Claude account assigned to this host at clone time (its
   * long-lived CLAUDE_CODE_OAUTH_TOKEN was written into ~/.bashrc and the
   * agent-wrapper restarted under it). Identifies which account on the usage
   * panel this host is spending. Server-only; the Rust client ignores it.
   */
  claudeAccountEmail?: string;
  /** Name of the Claude group this clone is balanced within, sticky until its account exhausts (null/absent = single account). */
  claudeGroup?: string;
  /**
   * The operator's Claude selection verbatim: "auto" (server picks the best account
   * and may hot-swap it), "none" (no token installed), "group:<name>", or an account
   * email (pinned). Distinguishes auto from a fixed account — `claudeAccountEmail`
   * alone can't. Absent on hosts created before this field / when Claude isn't used.
   */
  claudeSelection?: string;
  /** Linear workspace name this host's ticket belongs to (selects the card color). */
  linearWorkspace?: string;
  /** Linear ticket identifier, e.g. "WE-142". */
  linearTicket?: string;
  /** Full Linear ticket URL. */
  linearTicketUrl?: string;
  /** Linear's suggested git branch for the ticket. */
  linearBranch?: string;
  /** Ticket title — shown as the card's display name (falls back to `id`). */
  displayName?: string;
  /** First Linear label on the ticket, if any. */
  linearLabel?: string;
  /**
   * The agent's last self-reported desktop verdict (via the `set_state` MCP
   * tool): `working` (our agent or the in-editor Claude Code is actively working)
   * or `idle` (finished / awaiting the next task / needs a human). Folded into
   * `monitorState` by the poller.
   */
  agentReport?: "working" | "idle";
  /** Agent-authored note about the current state (what finished, or what the
   *  human needs to do/decide). Shown on the host card. */
  stateNote?: string;
  /**
   * Effective host state for the UI, derived by the server-side poller:
   * `offline` (wrapper unreachable) ▸ `working` (our agent has a turn in flight,
   * or the agent last reported working) ▸ `idle` (running but not working — the
   * agent finished or is awaiting the next task; absorbs the old "needs
   * attention"). A running host is just `working` or `idle`.
   */
  monitorState?: "working" | "idle" | "offline";
  /**
   * True when this clone dropped from `working` to idle/offline since it was
   * last viewed. Drives the sidebar unread dot; cleared when you switch to it.
   */
  unread?: boolean;
}

export type OperationKind = "clone" | "delete" | "pull" | "commit";
export type OperationStatus = "running" | "done" | "error";

export interface Operation {
  id: string;
  kind: OperationKind;
  /**
   * What the op acts on: host id (clone/delete) or image name (pull/commit).
   */
  target: string;
  /** Clone source image reference (clone), or source host id (commit). */
  source?: string;
  status: OperationStatus;
  /** Current step key (maps to a coarse percentage in the UI). */
  step: string;
  /** 0–100. */
  pct: number;
  message: string;
  /** Rolling log lines for the operation. */
  log: string[];
  startedAt: number;
  finishedAt?: number;
}

// --- Claude account usage (display-only) ---
//
// A non-secret per-account view derived by the usage poller and broadcast in
// `ControlState`. The OAuth tokens themselves live ONLY in the server-side
// secret store (data/claude-accounts.json) and never enter this struct, so they
// never ride the SSE frame to the web UI or the native RDP client.

export interface ClaudeUsageWindow {
  /** 0–100 utilization percent for the rolling window. */
  pct: number;
  /** ISO timestamp when the window resets, or null if unknown. */
  resetsAt: string | null;
}

export interface ClaudeSpend {
  /** Extra-usage credits spent this period, in cents. */
  usedCents: number;
  /** Monthly extra-usage limit in cents, or null if uncapped. */
  limitCents: number | null;
  /** 0–100 of the monthly limit. */
  pct: number;
  currency: string;
  resetsAt: string | null;
}

export interface ClaudeUsage {
  /** Stable account id: claude `${email}|${organizationUuid}`, codex `codex:<id>`. */
  id: string;
  email: string;
  /** Which provider this account belongs to (default treated as "claude"). */
  provider?: "claude" | "codex";
  /** True for the account claude-swap had active at import time. */
  active: boolean;
  /** True if this account can be picked when cloning a host — every imported
   *  Claude account (the server owns its token lifecycle). Codex accounts never. */
  assignable?: boolean;
  /** Set only when usage has NEVER been successfully fetched (no data to show). */
  error?: string;
  /** True when showing last-known-good data because the latest refresh failed
   *  (e.g. a transient 429). The window values are real but `lastUpdated` old. */
  stale?: boolean;
  /** ms epoch of the last SUCCESSFUL refresh (not the last attempt). */
  lastUpdated: number;
  fiveHour?: ClaudeUsageWindow;
  sevenDay?: ClaudeUsageWindow;
  spend?: ClaudeSpend;
}

export interface ControlState {
  selected: string | null;
  monitors: MonitorSpec[];
  hosts: Host[];
  operations: Operation[];
  /** Per-Claude-account usage view (no tokens). Refreshed by the usage poller. */
  claudeAccounts: ClaudeUsage[];
}

export function emptyState(): ControlState {
  return {
    selected: null,
    monitors: [],
    hosts: [],
    operations: [],
    claudeAccounts: [],
  };
}

// --- per-host chat (stored separately at data/chats/<id>.json, not in state) ---

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  text: string;
  ts: number;
}

export interface Chat {
  /**
   * Reserved. The agent-wrapper now owns session continuity in-memory, so the
   * control-server no longer tracks a session id here (kept for back-compat with
   * existing chat JSON files; always null on new writes).
   */
  sessionId: string | null;
  messages: ChatMessage[];
}

export function emptyChat(): Chat {
  return { sessionId: null, messages: [] };
}
