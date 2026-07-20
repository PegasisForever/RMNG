// Compact, card-less account-usage list under the group-proxy model, driven by
// ControlState.usageGroups (refreshed server-side, delivered over SSE). Display-only.
// Organized BY GROUP: each account pool (a CLIProxyAPI instance) is a header with a
// per-group "+ add account" (OAuth login) and delete control, and under it the accounts
// authenticated into that pool, each with 5h/7d/fable bars. The same email can appear
// under multiple groups (independent token sets) — that's expected.
//
// Each window's bar carries a vertical "pace" marker = the utilization you'd be at if
// you spent the quota uniformly across the window (elapsed fraction of
// [resetsAt - windowLength, resetsAt]); fill past the marker = burning faster than uniform.
import { Plus, RefreshCw, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";

import chatgptLogo from "../assets/chatgpt.svg";
import claudeLogo from "../assets/claude.svg";
import geminiLogo from "../assets/gemini.svg";
import type { ClaudeSpend, ClaudeUsage, ClaudeUsageWindow, GroupUsage } from "~/lib/types";

const FIVE_H_MS = 5 * 60 * 60 * 1000;
const SEVEN_D_MS = 7 * 24 * 60 * 60 * 1000;

/** Client-only clock (null during SSR) so the pace marker never causes hydration drift. */
function useNow(): number | null {
  const [now, setNow] = useState<number | null>(null);
  useEffect(() => {
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  return now;
}

function barColor(pct: number): string {
  if (pct >= 90) return "bg-rose-500";
  if (pct >= 70) return "bg-amber-500";
  return "bg-emerald-500";
}

/** Utilization expected at `now` if the window's quota were spent uniformly. */
function pacePct(resetsAt: string | null, windowMs: number, now: number): number | null {
  if (!resetsAt) return null;
  const reset = Date.parse(resetsAt);
  if (Number.isNaN(reset)) return null;
  const elapsed = windowMs - (reset - now); // ms since window start
  return Math.min(100, Math.max(0, (elapsed / windowMs) * 100));
}

function spendLine(spend: ClaudeSpend): string {
  const cur = spend.currency === "USD" ? "$" : `${spend.currency} `;
  const used = `${cur}${(spend.usedCents / 100).toFixed(2)}`;
  if (spend.limitCents == null) return used;
  return `${used}/${cur}${(spend.limitCents / 100).toFixed(2)}`;
}

function Bar({
  label,
  win,
  windowMs,
  now,
}: {
  label: string;
  win?: ClaudeUsageWindow;
  windowMs: number;
  now: number | null;
}) {
  if (!win) return null;
  const pct = Math.min(100, Math.max(0, win.pct));
  const pace = now != null ? pacePct(win.resetsAt, windowMs, now) : null;
  return (
    <div className="flex items-center gap-1.5">
      <span className="w-8 shrink-0 text-[10px] font-medium text-slate-500 dark:text-slate-400">{label}</span>
      <div className="relative h-1.5 flex-1 overflow-hidden rounded-sm bg-slate-200 dark:bg-slate-700">
        <div className={`h-full ${barColor(pct)}`} style={{ width: `${Math.max(1, pct)}%` }} />
        {pace != null ? (
          <div
            className="absolute top-0 h-full w-px bg-slate-900/70 dark:bg-slate-100/70"
            style={{ left: `${pace}%` }}
            title={`uniform pace ${Math.round(pace)}%`}
          />
        ) : null}
      </div>
      <span className="w-8 shrink-0 text-right text-[11px] font-medium tabular-nums text-slate-700 dark:text-slate-200">
        {pct}%
      </span>
    </div>
  );
}

function Row({ a, now }: { a: ClaudeUsage; now: number | null }) {
  const resetCredits =
    a.provider === "codex" && a.resetCredits != null ? Number(a.resetCredits) : null;
  return (
    <div className="px-1 py-1">
      <div className="flex items-center gap-1.5">
        <img
          src={
            a.provider === "codex"
              ? chatgptLogo
              : a.provider === "antigravity"
                ? geminiLogo
                : claudeLogo
          }
          alt={
            a.provider === "codex"
              ? "ChatGPT"
              : a.provider === "antigravity"
                ? "Gemini"
                : "Claude"
          }
          className={`h-3 w-3 shrink-0 rounded-[3px] object-contain ${
            a.provider === "codex" ? "dark:invert" : ""
          }`}
        />
        <span className="min-w-0 flex-1 truncate text-[11px] text-slate-700 dark:text-slate-200">
          {a.email}
        </span>
        {a.spend ? (
          <span className="shrink-0 text-[10px] tabular-nums text-slate-500 dark:text-slate-400">
            {spendLine(a.spend)}
          </span>
        ) : null}
        {resetCredits != null ? (
          <span
            className={`shrink-0 text-[10px] tabular-nums ${
              resetCredits === 0 ? "text-rose-400" : "text-slate-500 dark:text-slate-400"
            }`}
            title="Banked Codex rate-limit resets left"
          >
            ⟳ {resetCredits}
          </span>
        ) : null}
      </div>
      {a.provider === "antigravity" ? null : !a.fiveHour && !a.sevenDay && !a.fable ? (
        <div className="text-[10px] text-rose-400" title={a.error}>
          usage unavailable
        </div>
      ) : (
        <div
          className={`mt-0.5 space-y-0.5 ${a.stale ? "opacity-60" : ""}`}
          title={a.stale ? "stale — last refresh failed (showing last known)" : undefined}
        >
          <Bar label="5h" win={a.fiveHour} windowMs={FIVE_H_MS} now={now} />
          <Bar label="7d" win={a.sevenDay} windowMs={SEVEN_D_MS} now={now} />
          {/* Claude-only model-scoped weekly cap; a 7d window like sevenDay. Codex has none. */}
          <Bar label="fable" win={a.fable} windowMs={SEVEN_D_MS} now={now} />
        </div>
      )}
    </div>
  );
}

/** One account pool: a header (name + add-account + delete) and its authenticated accounts. */
function GroupBlock({
  group,
  now,
  onAddAccount,
  onDeleteGroup,
}: {
  group: GroupUsage;
  now: number | null;
  onAddAccount: (group: string) => void;
  onDeleteGroup: (group: string) => void;
}) {
  return (
    <div className="rounded border border-slate-200/70 dark:border-slate-700/70">
      <div className="flex items-center gap-1 border-b border-slate-200/70 px-1.5 py-1 dark:border-slate-700/70">
        <span className="min-w-0 flex-1 truncate text-[11px] font-semibold text-slate-600 dark:text-slate-300">
          {group.name}
        </span>
        <button
          type="button"
          onClick={() => onAddAccount(group.name)}
          title="Add an account to this group (OAuth login)"
          className="rounded p-0.5 text-slate-400 hover:bg-slate-100 hover:text-slate-600 dark:text-slate-500 dark:hover:bg-slate-800 dark:hover:text-slate-300"
        >
          <Plus className="size-3.5" />
        </button>
        <button
          type="button"
          onClick={() => {
            if (
              window.confirm(
                `Delete group "${group.name}"?\n\nStops its proxy instance. Clones bound to it lose inference until reassigned; the on-disk credentials are left in place.`,
              )
            )
              onDeleteGroup(group.name);
          }}
          title="Delete this group"
          className="rounded p-0.5 text-slate-400 hover:bg-red-50 hover:text-red-600 dark:text-slate-500 dark:hover:bg-red-950/40 dark:hover:text-red-400"
        >
          <Trash2 className="size-3.5" />
        </button>
      </div>
      {group.accounts.length === 0 ? (
        <button
          type="button"
          onClick={() => onAddAccount(group.name)}
          className="m-1 block w-[calc(100%-0.5rem)] rounded border border-dashed border-slate-300 px-2 py-1 text-[10px] text-slate-400 hover:bg-white dark:border-slate-600 dark:text-slate-500 dark:hover:bg-slate-800"
        >
          No accounts — add one
        </button>
      ) : (
        <div className="divide-y divide-slate-200/70 dark:divide-slate-700/70">
          {group.accounts.map((a) => (
            <Row key={a.id} a={a} now={now} />
          ))}
        </div>
      )}
    </div>
  );
}

export function ClaudeAccountsPanel({
  groups,
  onCreateGroup,
  onAddAccount,
  onDeleteGroup,
  onRefresh,
}: {
  /** Per-group usage view (from `ControlState.usageGroups`, merged with configured groups). */
  groups: GroupUsage[];
  /** Create a new account group. */
  onCreateGroup: () => void;
  /** Add an account to a group (opens the OAuth login flow). */
  onAddAccount: (group: string) => void;
  /** Delete a group. */
  onDeleteGroup: (group: string) => void;
  /** Trigger an immediate server-side usage poll (the refreshed view arrives over SSE). */
  onRefresh: () => void | Promise<void>;
}) {
  const now = useNow();
  const [refreshing, setRefreshing] = useState(false);
  const refresh = async () => {
    setRefreshing(true);
    try {
      await onRefresh();
    } finally {
      // Brief spin; the numbers themselves update when the poll's ControlState arrives over SSE.
      setTimeout(() => setRefreshing(false), 800);
    }
  };

  return (
    <div>
      <div className="flex items-center justify-between px-1">
        <h2 className="text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
          Groups{groups.length ? ` (${groups.length})` : ""}
        </h2>
        <div className="flex items-center gap-0.5">
          <button
            type="button"
            onClick={refresh}
            disabled={refreshing}
            title="Refresh usage now"
            className="rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600 disabled:opacity-50 dark:text-slate-500 dark:hover:bg-slate-800 dark:hover:text-slate-300"
          >
            <RefreshCw className={`size-3.5 ${refreshing ? "animate-spin" : ""}`} />
          </button>
          <button
            type="button"
            onClick={() => onCreateGroup()}
            title="Create an account group"
            className="rounded px-1 text-[11px] font-medium text-slate-400 hover:bg-slate-100 hover:text-slate-600 dark:text-slate-500 dark:hover:bg-slate-800 dark:hover:text-slate-300"
          >
            + Group
          </button>
        </div>
      </div>

      {groups.length === 0 ? (
        <button
          type="button"
          onClick={() => onCreateGroup()}
          className="mt-0.5 w-full rounded border border-dashed border-slate-300 px-2 py-1 text-[10px] text-slate-400 hover:bg-white dark:border-slate-600 dark:text-slate-500 dark:hover:bg-slate-800"
        >
          Create an account group
        </button>
      ) : (
        <div className="mt-0.5 space-y-1.5">
          {groups.map((g) => (
            <GroupBlock
              key={g.name}
              group={g}
              now={now}
              onAddAccount={onAddAccount}
              onDeleteGroup={onDeleteGroup}
            />
          ))}
        </div>
      )}
    </div>
  );
}
