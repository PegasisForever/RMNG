// Compact account usage, with one container per configured group, driven by
// ControlState.claudeAccounts (refreshed server-side every ~60s, delivered over
// SSE). Display-only. Each window's bar carries a vertical "pace" marker = the
// utilization you'd be at if you spent the quota uniformly across the window
// (elapsed fraction of [resetsAt - windowLength, resetsAt]); fill past the marker
// = burning faster than uniform.
import { Folder, RefreshCw } from "lucide-react";
import { useId, useState } from "react";

import chatgptLogo from "../assets/chatgpt.svg";
import claudeLogo from "../assets/claude.svg";
import { orderedWithinBuckets, type AcctOrder } from "~/lib/accountOrder";
import { resetTooltip } from "~/lib/format";
import type { ClaudeSpend, ClaudeUsage, ClaudeUsageWindow } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

const FIVE_H_MS = 5 * 60 * 60 * 1000;
const SEVEN_D_MS = 7 * 24 * 60 * 60 * 1000;

function barColor(pct: number): string {
  if (pct >= 90) return "bg-rose-500";
  if (pct >= 70) return "bg-amber-500";
  return "bg-emerald-500";
}

/** Utilization expected at `now` if the window's quota were spent uniformly. */
function pacePct(
  resetsAt: string | null,
  windowMs: number,
  now: number,
): number | null {
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
  locale,
}: {
  label: string;
  win?: ClaudeUsageWindow;
  windowMs: number;
  now: number | null;
  locale: string;
}) {
  if (!win) return null;
  const pct = Math.min(100, Math.max(0, win.pct));
  const pace = now == null ? null : pacePct(win.resetsAt, windowMs, now);
  // Concrete reset time in the viewer's local zone. Gated on the client clock (`now`, null
  // until the effect runs) for the same reason as the pace marker: the string is rendered in
  // the BROWSER's time zone, which the prerender has no way to know, so computing it during
  // the first render would guarantee a hydration mismatch. Do not "simplify" this to
  // `resetTooltip(win.resetsAt, Date.now(), navigator.language)`.
  const resetTitle =
    now == null ? null : resetTooltip(win.resetsAt, now, locale);
  return (
    <div className="flex items-center gap-1.5" title={resetTitle ?? undefined}>
      <span className="w-6 shrink-0 text-[10px] font-medium text-slate-500 dark:text-slate-400">
        {label}
      </span>
      <div className="relative h-1.5 flex-1 overflow-hidden rounded-sm bg-slate-200 dark:bg-slate-700">
        <div
          className={`h-full ${barColor(pct)}`}
          style={{ width: `${Math.max(1, pct)}%` }}
        />
        {pace == null ? null : (
          <div
            className="absolute top-0 h-full w-px bg-slate-900/70 dark:bg-slate-100/70"
            style={{ left: `${pace}%` }}
            title={`uniform pace ${Math.round(pace)}%`}
          />
        )}
      </div>
      <span className="w-8 shrink-0 text-right text-[11px] font-medium tabular-nums text-slate-700 dark:text-slate-200">
        {pct}%
      </span>
    </div>
  );
}

/** One rendered section: a configured pool (both providers' members together), or one
 *  provider's leftovers that belong to no pool. */
export interface AccountSection {
  /** Pool name, or null for the accounts no pool claims. */
  name: string | null;
  /** Leftover sections are per provider; pool sections always read "claude" (vestigial —
   *  a pool holds both providers now, and pool names are unique). */
  provider: "claude" | "codex";
  accounts: ClaudeUsage[];
}

/** Split accounts into their pools, keeping `ordered`'s order inside each one.
 *
 *  An account in several pools appears in each of them: the pools are how a clone's binding
 *  is resolved, so seeing an account once under "Medi" and again under "Personal" is the
 *  point, not a duplicate to collapse.
 *
 *  A configured pool with no accounts still renders. An empty pool is a misconfiguration
 *  worth seeing, and it is exactly the state that leaves clones bound to it unassigned. */
export function groupAccounts(
  ordered: ClaudeUsage[],
  groups: CloneGroup[],
): AccountSection[] {
  const out: AccountSection[] = [];
  const byEmail = new Map(ordered.map((a) => [a.email, a]));
  const claimed = new Set<string>();
  for (const group of groups) {
    // One section per pool holding BOTH providers' members: a pool is one binding now,
    // so its Claude and Codex members belong on screen together. Member order is the
    // pool's own order — the same array the settings tree drags around — not the
    // cosmetic account order. A member with no imported row (stale draft email) draws
    // nothing rather than an empty row.
    const accounts: ClaudeUsage[] = [];
    for (const email of group.accounts) {
      const row = byEmail.get(email);
      if (row) {
        accounts.push(row);
        claimed.add(row.id);
      }
    }
    out.push({ name: group.name, provider: "claude", accounts });
  }
  for (const provider of ["claude", "codex"] as const) {
    const loose = ordered.filter(
      (a) => (a.provider ?? "claude") === provider && !claimed.has(a.id),
    );
    if (loose.length > 0) out.push({ name: null, provider, accounts: loose });
  }
  return out;
}

function Row({
  a,
  now,
  locale,
  onReplace,
}: {
  a: ClaudeUsage;
  now: number | null;
  locale: string;
  onReplace: (account: ClaudeUsage) => void;
}) {
  const resetCredits =
    a.provider === "codex" && a.resetCredits != null
      ? Number(a.resetCredits)
      : null;
  return (
    <div className="px-2 py-1">
      <div className="flex items-center gap-1.5">
        <img
          src={a.provider === "codex" ? chatgptLogo : claudeLogo}
          alt={a.provider === "codex" ? "ChatGPT" : "Claude"}
          // The ChatGPT mark ships with no `fill`, so it paints black and vanishes on a dark
          // background — invert it there. `claude.svg` carries its own fill, so inverting it
          // too would only wreck a logo that already reads fine.
          className={`h-3 w-3 shrink-0 rounded-[3px] object-contain ${
            a.provider === "codex" ? "dark:invert" : ""
          }`}
        />
        <span
          title={a.email}
          className="min-w-0 flex-1 truncate text-[11px] font-medium text-slate-700 dark:text-slate-200"
        >
          {a.email}
        </span>
        {/* The account holds no token that still works, so the rotator has taken it out and
            no clone can run on it until someone signs in again. Dimmed bars and a tooltip
            were the only sign of this before, which is how one dead account ran a third of
            the fleet on an expired token for ten hours.

            It is the fix as well as the diagnosis. As a label it named a repair the operator
            then had to carry out by hand — delete the account, import its replacement, and
            rebuild its pools and pins from memory. */}
        {a.assignable === false ? (
          <button
            type="button"
            onClick={() => onReplace(a)}
            title={`${a.error ?? "the stored token expired and could not be refreshed"}\n\nSign in to replace this account.`}
            className="shrink-0 rounded bg-rose-100 px-1 text-[10px] font-medium text-rose-600 hover:bg-rose-200 dark:bg-rose-950/60 dark:text-rose-400 dark:hover:bg-rose-900/60"
          >
            sign in again
          </button>
        ) : null}
        {a.spend ? (
          <span className="shrink-0 text-[10px] tabular-nums text-slate-500 dark:text-slate-400">
            {spendLine(a.spend)}
          </span>
        ) : null}
        {resetCredits == null ? null : (
          <span
            className={`shrink-0 text-[10px] tabular-nums ${
              resetCredits === 0
                ? "text-rose-400"
                : "text-slate-500 dark:text-slate-400"
            }`}
            title="Banked Codex rate-limit resets left"
          >
            ⟳ {resetCredits}
          </span>
        )}
      </div>
      {!a.fiveHour && !a.sevenDay && !a.fable ? (
        <div className="ml-[18px] text-[10px] text-rose-400" title={a.error}>
          usage unavailable
        </div>
      ) : (
        <div
          className={`ml-[18px] mt-0.5 ${a.stale ? "opacity-60" : ""}`}
          title={
            a.stale
              ? "stale — last refresh failed (showing last known)"
              : undefined
          }
        >
          <Bar
            label="5h"
            win={a.fiveHour}
            windowMs={FIVE_H_MS}
            now={now}
            locale={locale}
          />
          <Bar
            label="7d"
            win={a.sevenDay}
            windowMs={SEVEN_D_MS}
            now={now}
            locale={locale}
          />
          {/* Claude-only model-scoped weekly cap; a 7d window like sevenDay. Codex has none. */}
          <Bar
            label="fable"
            win={a.fable}
            windowMs={SEVEN_D_MS}
            now={now}
            locale={locale}
          />
        </div>
      )}
    </div>
  );
}

export function ClaudeAccountsPanel({
  accounts,
  accountOrder,
  groups = [],
  locale,
  now,
  onRefresh,
  onReplace,
}: {
  accounts: ClaudeUsage[];
  /** The cosmetic order the operator dragged out in Settings, per provider. The container
   *  subscribes to the shared store and hands the value down, which is what keeps this panel
   *  and the Settings lists in step without either one reading the store during render. */
  accountOrder: AcctOrder;
  /** The single configured pool list (`config.groups`). With none, the list stays flat. */
  groups?: CloneGroup[];
  /** Formats each bar's reset-time tooltip. Read on the container's side of the seam
   *  (`navigator.language`) and passed down, so the panel draws the same string for the same
   *  props on every machine and a story can pin it. */
  locale: string;
  /** Wall-clock milliseconds, driving both things a `resetsAt` decides: each bar's pace
   *  marker and its reset tooltip. Null before the container's clock has ticked, which draws
   *  neither. Injected for the same reason as `locale`: a panel that read the clock itself
   *  would draw a different picture every time a story was opened. */
  now: number | null;
  onRefresh: () => void | Promise<void>;
  /** Sign in to an account that takes over from this dead one. Reached from the "sign in
   *  again" badge, which only draws on a row the server has marked unassignable. Adding
   *  accounts lives in Settings (each pool has its own import buttons) — this column
   *  deliberately offers no add entry point. */
  onReplace: (account: ClaudeUsage) => void;
}) {
  const rows = orderedWithinBuckets(
    accounts,
    (a) => a.provider ?? "claude",
    (a) => a.id,
    accountOrder,
  );
  // With no pools configured there is nothing to group by, so the list stays flat.
  const sections = groups.length > 0 ? groupAccounts(rows, groups) : [];
  const headingId = useId();
  const [busy, setBusy] = useState(false);
  const wrap = (fn: () => void | Promise<void>) => async () => {
    setBusy(true);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  };

  return (
    <section aria-labelledby={headingId}>
      <div className="mb-1 flex items-center justify-between">
        <h2
          id={headingId}
          className="text-xs font-semibold text-slate-700 dark:text-slate-200"
        >
          Account usage
        </h2>
        {accounts.length > 0 ? (
          <button
            type="button"
            onClick={wrap(onRefresh)}
            disabled={busy}
            title="Refresh usage now"
            aria-label="Refresh usage now"
            className="rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600 disabled:opacity-50 dark:text-slate-500 dark:hover:bg-slate-800 dark:hover:text-slate-300"
          >
            {busy ? "…" : <RefreshCw className="size-4" />}
          </button>
        ) : null}
      </div>

      {accounts.length === 0 ? (
        <p className="mt-0.5 rounded border border-dashed border-slate-300 px-2 py-1 text-[10px] text-slate-400 dark:border-slate-600 dark:text-slate-500">
          No accounts — import one in Settings → LLM.
        </p>
      ) : sections.length === 0 ? (
        <div className="divide-y divide-slate-100 overflow-hidden rounded-lg border border-slate-200 bg-white dark:divide-slate-800 dark:border-slate-700 dark:bg-slate-900">
          {rows.map((a) => (
            <Row
              key={a.id}
              a={a}
              now={now}
              locale={locale}
              onReplace={onReplace}
            />
          ))}
        </div>
      ) : (
        <div className="space-y-1.5">
          {sections.map((section) => (
            <section
              key={`${section.provider}|${section.name ?? ""}`}
              aria-label={
                section.name ??
                `Ungrouped ${section.provider === "codex" ? "Codex" : "Claude"}`
              }
              className="overflow-hidden rounded-lg border border-slate-200 bg-white dark:border-slate-700 dark:bg-slate-900"
            >
              <h3 className="flex items-center gap-1.5 border-b border-slate-200 bg-slate-100/80 px-2 py-1 text-[11px] font-semibold text-slate-600 dark:border-slate-700 dark:bg-slate-800/80 dark:text-slate-300">
                <Folder
                  aria-hidden="true"
                  className="size-3 shrink-0 text-slate-400 dark:text-slate-500"
                />
                <span className="min-w-0 break-words">
                  {section.name ??
                    `Ungrouped ${section.provider === "codex" ? "Codex" : "Claude"}`}
                </span>
              </h3>
              {section.accounts.length === 0 ? (
                <p className="px-2 py-2 text-[11px] text-slate-500 dark:text-slate-400">
                  No accounts
                </p>
              ) : (
                <div className="divide-y divide-slate-100 dark:divide-slate-800">
                  {section.accounts.map((a) => (
                    // An account in two pools renders in both, so the pool has to be part of
                    // the key.
                    <Row
                      key={`${section.provider}|${section.name ?? ""}|${a.id}`}
                      a={a}
                      now={now}
                      locale={locale}
                      onReplace={onReplace}
                    />
                  ))}
                </div>
              )}
            </section>
          ))}
        </div>
      )}
    </section>
  );
}
