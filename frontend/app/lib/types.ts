// Shared types for the control plane.
//
// `ControlState` is a superset of the legacy Rust `ControlState`
// (`{selected, hosts, monitors}`). The native RDP client deserializes the SSE
// payload as that Rust struct, and serde ignores unknown fields — so the extra
// `operations` field (and the per-clone `container`/`source` extras) ride along
// invisibly. In-progress clones are kept OUT of `hosts` until they are fully
// provisioned, so the client never tries to connect to a half-built container.

import type { CloneTokens } from "~/lib/wire/CloneTokens";
import type { PortForward } from "~/lib/wire/PortForward";

export interface MonitorSpec {
  width: number;
  height: number;
}

export interface Clone {
  /** Stable id; equals the Docker container name for a managed clone. */
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
   * True for a managed clone: a Docker container whose *name equals this clone's
   * id* backs it (no container id is stored anywhere). False/absent is a plain
   * unmanaged row (deletable in the UI). Old `state.json` rows carrying the
   * retired `ctid`/`container` keys load unmanaged — serde drops the stale keys.
   */
  managed?: boolean;
  /** True for a retained managed clone that is intentionally stopped. Unlike `offline`,
   *  this is an operator-controlled state and the clone can be started again. */
  archived?: boolean;
  /** The clone-source image reference this clone was cloned from (`pegasis0/rmng-template:latest`). */
  source?: string;
  /** Email of the imported Claude account whose access token the server has installed into
   *  this clone's `~/.claude/.credentials.json`. Absent when no account is assigned. */
  claudeAccountEmail?: string;
  /** Name of the Claude account pool this clone is balanced within (sticky — it moves only
   *  when its account exhausts). Absent when pinned to one fixed account. */
  claudeGroup?: string;
  /** The operator's Claude selection verbatim: `auto`, `none`, `group:<pool>`, or an email.
   *  Distinguishes an auto-managed clone from one pinned or deliberately tokenless —
   *  `claudeAccountEmail` alone cannot tell those apart. */
  claudeSelection?: string;
  /** The Codex twins of the three above. Independent — a clone can hold both providers. */
  codexAccountEmail?: string;
  codexGroup?: string;
  codexSelection?: string;
  /** Linear workspace name this clone's ticket belongs to (selects the card color). */
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
   * Server-owned lifecycle state: Docker supplies liveness and passive proxy token activity
   * distinguishes working from idle. The sidebar uses this only for its compact indicator.
   */
  monitorState?: "working" | "idle" | "offline";
  /** Set when an unselected clone transitions from working to idle/offline. */
  unread?: boolean;
  /** Headless clone: no desktop (display + capture units disabled at create). Selecting it
   *  shows the viewer's tmux tab view instead of a video stream. Same template as a regular clone. */
  headless?: boolean;
  /** Parent clone id when this is a sub clone (one level deep only). Undefined/null = top-level.
   *  Cosmetic sidebar/`ps` grouping; a sub clone is otherwise an ordinary managed clone. */
  parent?: string | null;
  /** Local port-forward rules; the native viewer runs the listeners. Live status
   *  arrives separately via the `forwards` SSE event, keyed by clone id then rule id. */
  forwards?: PortForward[];
}

export type OperationKind =
  | "clone"
  | "delete"
  | "archive"
  | "unarchive"
  | "pull"
  | "commit"
  | "update";
export type OperationStatus = "running" | "done" | "error";

export interface Operation {
  id: string;
  kind: OperationKind;
  /**
   * What the op acts on: clone id (clone/delete) or image name (pull/commit).
   */
  target: string;
  /** Clone source image reference (clone), or source clone id (commit). Serialized as
   *  `null` when absent (the wire type is `Option<String>` with no skip), so accept both. */
  source?: string | null;
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
  /** True if this account can be picked when creating a clone — every imported
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
  /** Claude only: the model-scoped weekly (7d) limit for the Fable model family.
   *  Display-only — it never affects account rotation. Absent for Codex accounts and
   *  when the account has no such scoped limit. */
  fable?: ClaudeUsageWindow;
  spend?: ClaudeSpend;
  /** Codex only: banked rate-limit reset credits ("usage resets") left on the
   *  account. Absent/null for Claude (no such concept) and when usage is
   *  unavailable. Wire type is `bigint | null` — callers must `Number(...)`
   *  it before numeric comparison. */
  resetCredits?: bigint | null;
}

export interface ControlState {
  selected: string | null;
  monitors: MonitorSpec[];
  hosts: Clone[];
  operations: Operation[];
  /** Per-account usage view (no tokens). Despite the name it carries BOTH providers' rows,
   *  tagged by `ClaudeUsage.provider`. Refreshed by the two account pollers. */
  claudeAccounts: ClaudeUsage[];
  /** Name of the currently active layout preset. */
  activeLayout: string;
  /** Layout preset names in config order — powers the sidebar switcher. */
  layoutPresetNames: string[];
  /** All-time per-clone token totals, keyed by clone id. Persisted server-side (the agent
   *  logs it is derived from get pruned), so it arrives with the ordinary state snapshot
   *  rather than on a volatile bus like `stats`. */
  cloneTokens: Record<string, CloneTokens>;
}

export function emptyState(): ControlState {
  return {
    selected: null,
    monitors: [],
    hosts: [],
    operations: [],
    claudeAccounts: [],
    activeLayout: "",
    layoutPresetNames: [],
    cloneTokens: {},
  };
}

// --- per-clone chat (stored separately at data/chats/<id>.json, not in state) ---

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
