// The phone's home screen: account usage over a flat list of clones. It is the whole app
// on a phone — tapping a row opens MobileClone, and there is nowhere else to go.
//
// Pure, like AppShellV2: no fetch, no SSE, no browser-only imports. It also fills its
// container rather than the viewport, so the route wraps it in `100dvh` and a story can
// put it in a phone-sized frame.
//
// The board's columns survive as section headers. Their order and membership are the
// operator's own filing, so the phone reads it rather than inventing a sort — but nothing
// here moves a card between them, which is why the rows are plain list items and not cards.
//
// Two things the desktop has are deliberately absent. There is no create button: a phone
// answers work, it does not start it. And there is no settings gear, because every setting
// behind it needs a keyboard and a wide screen.
import { ChevronRight, ChevronDown } from "lucide-react";

import { ClaudeAccountsPanel } from "~/components/ClaudeAccountsPanel";
import { CloneStatusDot } from "~/components/mobile/CloneStatus";
import type { AcctOrder } from "~/lib/accountOrder";
import { resolveColumns, withDefaults, type BoardColumn } from "~/lib/board";
import type { ClaudeUsage, Clone } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

/** The single worst usage window across every account, which is what the collapsed usage
 *  row reports. */
export interface UsagePeak {
  pct: number;
  email: string;
  window: "5h" | "7d";
}

/** The highest rolling-window utilization anyone is at, or null with no accounts (or no
 *  window data yet).
 *
 *  Only the 5h and 7d windows count. The Fable window is display-only on the desktop panel
 *  and never gates a clone, so letting it drive the one number on the phone would raise an
 *  alarm about a limit nothing is waiting on. Ties go to the first account in the given
 *  order, which is the operator's own ordering from Settings. */
export function peakUsage(accounts: ClaudeUsage[]): UsagePeak | null {
  let peak: UsagePeak | null = null;
  for (const a of accounts) {
    const windows: [UsagePeak["window"], number | undefined][] = [
      ["5h", a.fiveHour?.pct],
      ["7d", a.sevenDay?.pct],
    ];
    for (const [window, pct] of windows) {
      if (pct === undefined) continue;
      if (!peak || pct > peak.pct) peak = { pct, email: a.email, window };
    }
  }
  return peak;
}

export interface MobileHomeProps {
  /** Per-account usage rows, both providers, from `ControlState.claudeAccounts`. */
  accounts: ClaudeUsage[];
  /** The operator's own ordering of those rows, dragged out in desktop Settings. Read from
   *  the shared store by the dashboard container and handed down. */
  accountOrder: AcctOrder;
  /** Configured Claude pools (`config.cloneGroups`) — the usage list groups by these. */
  cloneGroups?: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups?: CloneGroup[];
  /** Formats the usage bars' reset-time tooltips. Read once by the route (the operator's
   *  `navigator.language`) and handed down, so a story can pin it. */
  locale: string;
  /** Wall-clock milliseconds for the usage bars' pace markers and reset tooltips. Read once
   *  by the dashboard container, null until its clock has ticked. */
  now: number | null;
  /** The usage section is collapsed by default: three accounts of bars would push the
   *  clone list off a phone screen, and the peak number answers the usual question. */
  usageOpen: boolean;
  onUsageOpenChange: (open: boolean) => void;
  /** Refresh usage now. The panel draws the button. */
  onRefresh: () => void | Promise<void>;
  /** Import an account from a clone that is already signed in. */
  onImportAccount: () => void | Promise<void>;
  /** The board's columns (`ControlState.boardColumns`), which become the section headers.
   *  Empty falls back to the same defaults the desktop board uses. */
  columns: BoardColumn[];
  /** Every clone to list. Their order inside a section comes from the column, not from
   *  here, and anything unfiled lands in its home column the way the board draws it. */
  clones: Clone[];
  onSelectClone: (clone: Clone) => void;
  /** Last failed action, shown as a banner under the header. */
  error?: string | null;
}

/** Ticket and account, the two things that say which clone this is when several share a
 *  shape. Empty when the clone has neither, and the row then shows its id instead. */
function subtitle(clone: Clone): string {
  return [clone.linearTicket, clone.claudeAccountEmail].filter(Boolean).join(" · ");
}

function CloneRow({ clone, onSelect }: { clone: Clone; onSelect: (clone: Clone) => void }) {
  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect(clone)}
        className="flex min-h-16 w-full items-center gap-3 px-4 py-3 text-left active:bg-slate-100 dark:active:bg-slate-800"
      >
        <CloneStatusDot clone={clone} />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium text-slate-800 dark:text-slate-100">
            {clone.displayName ?? clone.id}
          </span>
          <span className="block truncate text-xs text-slate-500 dark:text-slate-400">
            {subtitle(clone) || clone.id}
          </span>
        </span>
        <ChevronRight aria-hidden className="size-4 shrink-0 text-slate-300 dark:text-slate-600" />
      </button>
    </li>
  );
}

export function MobileHome({
  accounts,
  accountOrder,
  cloneGroups = [],
  codexGroups = [],
  locale,
  now,
  usageOpen,
  onUsageOpenChange,
  onRefresh,
  onImportAccount,
  columns,
  clones,
  onSelectClone,
  error = null,
}: MobileHomeProps) {
  const peak = peakUsage(accounts);
  const byId = new Map(clones.map((c) => [c.id, c]));
  // Empty columns are dropped rather than drawn as empty headers. On the desktop an empty
  // column is a drop target worth keeping on screen; here nothing can be dropped, so it
  // would only cost a phone screen's scarcest thing.
  const sections = resolveColumns(withDefaults(columns), clones)
    .map((column) => ({
      column,
      rows: column.cloneIds.flatMap((id) => {
        const clone = byId.get(id);
        return clone ? [clone] : [];
      }),
    }))
    .filter((section) => section.rows.length > 0);

  return (
    // No title bar. The app has one screen and one name, so a bar carrying only "rmng"
    // would spend a phone's scarcest space saying what the home screen icon already said.
    // The notch inset moves here instead, which also keeps the sticky section headers
    // pinning below it rather than under it.
    <div className="flex h-full min-h-0 flex-col bg-slate-100 pt-[env(safe-area-inset-top)] dark:bg-slate-950">
      {error ? (
        <p
          role="alert"
          className="shrink-0 bg-rose-50 px-4 py-2 text-sm text-rose-700 dark:bg-rose-950 dark:text-rose-300"
        >
          {error}
        </p>
      ) : null}

      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <section className="border-b border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900">
          <button
            type="button"
            onClick={() => onUsageOpenChange(!usageOpen)}
            aria-expanded={usageOpen}
            className="flex min-h-14 w-full items-center gap-3 px-4 py-3 text-left active:bg-slate-100 dark:active:bg-slate-800"
          >
            <span className="text-sm font-medium text-slate-700 dark:text-slate-200">Usage</span>
            <span className="min-w-0 flex-1 truncate text-right text-xs tabular-nums text-slate-500 dark:text-slate-400">
              {peak
                ? `${peak.pct}% ${peak.window} · ${peak.email}`
                : accounts.length
                  ? "no data yet"
                  : "no accounts"}
            </span>
            <ChevronDown
              aria-hidden
              className={`size-4 shrink-0 text-slate-400 transition-transform ${usageOpen ? "rotate-180" : ""}`}
            />
          </button>
          {usageOpen ? (
            <div className="px-3 pb-3">
              <ClaudeAccountsPanel
                accounts={accounts}
                accountOrder={accountOrder}
                cloneGroups={cloneGroups}
                codexGroups={codexGroups}
                locale={locale}
                now={now}
                onRefresh={onRefresh}
                onImport={onImportAccount}
              />
            </div>
          ) : null}
        </section>

        {sections.length === 0 ? (
          <p className="px-4 py-10 text-center text-sm text-slate-500 dark:text-slate-400">
            No clones. Create one from the desktop.
          </p>
        ) : (
          sections.map(({ column, rows }) => (
            <section key={column.id}>
              {/* Sticky, so the column you are scrolling through keeps saying which one it
                  is. It pins to the top of this scroller, which the usage section shares. */}
              <h2 className="sticky top-0 z-10 flex items-baseline gap-2 bg-slate-100/95 px-4 py-1.5 text-[11px] font-semibold uppercase tracking-wide text-slate-500 backdrop-blur dark:bg-slate-950/95 dark:text-slate-400">
                <span className="truncate">{column.title}</span>
                <span className="tabular-nums text-slate-400 dark:text-slate-500">
                  {rows.length}
                </span>
              </h2>
              <ul className="divide-y divide-slate-200 bg-white dark:divide-slate-800 dark:bg-slate-900">
                {rows.map((clone) => (
                  <CloneRow key={clone.id} clone={clone} onSelect={onSelectClone} />
                ))}
              </ul>
            </section>
          ))
        )}
      </div>
    </div>
  );
}
