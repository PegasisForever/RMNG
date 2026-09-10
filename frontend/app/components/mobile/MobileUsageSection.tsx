// The phone home screen's usage disclosure: one number when it is folded, the desktop's own
// account panel when it is open.
//
// Collapsed by default, and the collapsed line is the whole reason this is a disclosure: three
// accounts of bars would push the clone list off a phone screen, while the peak number answers
// the question that made anyone look.
//
// The panel inside it is the desktop's `ClaudeAccountsPanel`, unchanged. Its clock and its
// locale are props here too, so a story pins them and every bar draws the same thing for the
// same props.
import { ChevronDown } from "lucide-react";

import { ClaudeAccountsPanel } from "~/components/ClaudeAccountsPanel";
import type { AcctOrder } from "~/lib/accountOrder";
import type { ClaudeUsage } from "~/lib/types";
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

export interface MobileUsageSectionProps {
  /** Per-account usage rows, both providers, from `ControlState.claudeAccounts`. */
  accounts: ClaudeUsage[];
  /** The operator's own ordering of those rows, dragged out in desktop Settings. Read from
   *  the shared store by the dashboard container and handed down. */
  accountOrder: AcctOrder;
  /** The single configured pool list (`config.groups`) — the usage list groups by these. */
  groups?: CloneGroup[];
  /** Formats the usage bars' reset-time tooltips. Read once by the route (the operator's
   *  `navigator.language`) and handed down, so a story can pin it. */
  locale: string;
  /** Wall-clock milliseconds for the usage bars' pace markers and reset tooltips. Read once
   *  by the dashboard container, null until its clock has ticked. */
  now: number | null;
  /** The section is expanded. */
  usageOpen: boolean;
  onUsageOpenChange: (open: boolean) => void;
  /** Refresh usage now. The panel draws the button. */
  onRefresh: () => void | Promise<void>;
  /** Import an account from a clone that is already signed in. The container owns the dialog
   *  this opens. */
  /** Sign in to an account that takes over from a dead one (the "sign in again" badge). */
  onReplaceAccount: (account: ClaudeUsage) => void;
}

export function MobileUsageSection({
  accounts,
  accountOrder,
  groups = [],
  locale,
  now,
  usageOpen,
  onUsageOpenChange,
  onRefresh,
  onReplaceAccount,
}: MobileUsageSectionProps) {
  const peak = peakUsage(accounts);

  return (
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
            groups={groups}
            locale={locale}
            now={now}
            onRefresh={onRefresh}
            onReplace={onReplaceAccount}
          />
        </div>
      ) : null}
    </section>
  );
}
