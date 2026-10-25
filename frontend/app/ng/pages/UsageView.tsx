// The /ng Usage page: every account's usage, in the pools the dashboard's rail shows, and the
// jobs the server is running. On the desktop the reset time of a window is a tooltip; a touch
// screen has no hover, so here it is on the bar, as "in 2h 15m".
//
// Pure: the accounts, the clock and the locale come in as props, and the refresh and the
// sign-in go out as callbacks. The server's CPU, memory and disk are in the tool row above.
import { Folder, RefreshCw } from "lucide-react";
import { useState } from "react";

import chatgptLogo from "~/assets/chatgpt.svg";
import claudeLogo from "~/assets/claude.svg";
import {
  barColor,
  FIVE_H_MS,
  groupAccounts,
  pacePct,
  SEVEN_D_MS,
  spendLine,
} from "~/components/ClaudeAccountsPanel";
import { OperationProgress } from "~/components/OperationProgress";
import { type AcctOrder, orderedWithinBuckets } from "~/lib/accountOrder";
import { until } from "~/lib/format";
import type { ClaudeUsage, ClaudeUsageWindow, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { SoundReport } from "~/viewer/protocol";

import { EmptyLine, HeaderButton, Page, PageHeader } from "./ui";

export interface UsageViewProps {
  /** Both providers' accounts (`ControlState.claudeAccounts`). */
  accounts: ClaudeUsage[];
  /** The operator's order of them, from desktop Settings. */
  accountOrder: AcctOrder;
  /** The pools; with none the list is one flat group. */
  groups: CloneGroup[];
  /** Wall-clock ms for the pace marks and the reset times; null until the clock ticked. */
  now: number | null;
  /** All operations; the running ones show at the top. */
  operations: Operation[];
  /** How the clone's sound plays on this phone (the line at the bottom); null before it plays. */
  sound?: SoundReport | null;
  onRefresh: () => void | Promise<void>;
  /** Sign in to an account that takes over from this dead one. */
  onReplaceAccount: (account: ClaudeUsage) => void;
}

export function UsageView({
  accounts,
  accountOrder,
  groups,
  now,
  operations,
  sound = null,
  onRefresh,
  onReplaceAccount,
}: UsageViewProps) {
  const [busy, setBusy] = useState(false);
  const refresh = async () => {
    setBusy(true);
    try {
      await onRefresh();
    } finally {
      setBusy(false);
    }
  };
  const rows = orderedWithinBuckets(accounts, (a) => a.provider ?? "claude", (a) => a.id, accountOrder);
  const sections =
    groups.length > 0 ? groupAccounts(rows, groups) : [{ name: null, provider: "claude" as const, accounts: rows }];
  const running = operations
    .filter((o) => o.status === "running")
    .sort((a, b) => Number(b.startedAt) - Number(a.startedAt));

  return (
    <Page
      header={
        <PageHeader title="Account usage" sub={accounts.length ? `${accounts.length} accounts` : undefined}>
          {accounts.length ? (
            <HeaderButton icon={RefreshCw} label={busy ? "Refreshing…" : "Refresh"} showLabel disabled={busy} onClick={refresh} />
          ) : null}
        </PageHeader>
      }
    >
      <div className="space-y-3 p-2">
        {running.length ? (
          <section aria-label="Running jobs" className="space-y-1.5">
            <h3 className="px-1 text-[11px] font-medium text-neutral-500">Running jobs · {running.length}</h3>
            <div className="grid grid-cols-2 gap-1.5">
              {running.map((op) => (
                <OperationProgress key={op.id} op={op} />
              ))}
            </div>
          </section>
        ) : null}
        {accounts.length === 0 ? (
          <EmptyLine>No accounts. Import one in Settings, LLM.</EmptyLine>
        ) : (
          sections.map((section) => {
            const name =
              section.name ?? (groups.length ? `Ungrouped ${section.provider === "codex" ? "Codex" : "Claude"}` : null);
            return (
              <section key={`${section.provider}|${section.name ?? ""}`} aria-label={name ?? "Accounts"}>
                {name ? (
                  <h3 className="flex items-center gap-1.5 px-1 pb-1 text-[11px] font-medium text-neutral-400">
                    <Folder aria-hidden className="size-3 text-neutral-500" />
                    {name}
                  </h3>
                ) : null}
                {section.accounts.length === 0 ? (
                  <p className="px-1 text-xs text-neutral-500">No accounts</p>
                ) : (
                  <div className="grid grid-cols-2 gap-1.5">
                    {section.accounts.map((a) => (
                      <AccountCard key={a.id} a={a} now={now} onReplace={onReplaceAccount} />
                    ))}
                  </div>
                )}
              </section>
            );
          })
        )}
        {sound ? <SoundLine r={sound} /> : null}
      </div>
    </Page>
  );
}

/** One account: who, what it costs, and a bar per usage window with its reset time. */
export function AccountCard({
  a,
  now,
  onReplace,
}: {
  a: ClaudeUsage;
  now: number | null;
  onReplace: (account: ClaudeUsage) => void;
}) {
  const codex = a.provider === "codex";
  const credits = codex && a.resetCredits != null ? Number(a.resetCredits) : null;
  const empty = !a.fiveHour && !a.sevenDay && !a.fable;
  return (
    <article className="min-w-0 rounded-lg bg-neutral-900 px-2.5 py-2">
      <div className="flex items-center gap-1.5">
        <img
          src={codex ? chatgptLogo : claudeLogo}
          alt={codex ? "ChatGPT" : "Claude"}
          className={`size-3.5 shrink-0 object-contain ${codex ? "invert" : ""}`}
        />
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-neutral-100">{a.email}</span>
        {a.spend ? <span className="shrink-0 text-[11px] tabular-nums text-neutral-400">{spendLine(a.spend)}</span> : null}
        {credits == null ? null : (
          <span className={`shrink-0 text-[11px] tabular-nums ${credits === 0 ? "text-rose-400" : "text-neutral-400"}`}>
            ⟳ {credits}
          </span>
        )}
      </div>
      {a.assignable === false ? (
        <button
          type="button"
          onClick={() => onReplace(a)}
          className="mt-1.5 w-full rounded-md bg-rose-950 py-1.5 text-xs font-medium text-rose-200 active:bg-rose-900"
        >
          Token expired. Sign in again
        </button>
      ) : null}
      {empty ? (
        <p className="mt-1 text-[11px] text-rose-400">Usage not available{a.error ? `: ${a.error}` : ""}</p>
      ) : (
        <div className={`mt-1.5 space-y-1 ${a.stale ? "opacity-60" : ""}`}>
          <UsageBar label="5h" win={a.fiveHour} windowMs={FIVE_H_MS} now={now} />
          <UsageBar label="7d" win={a.sevenDay} windowMs={SEVEN_D_MS} now={now} />
          <UsageBar label="fable" win={a.fable} windowMs={SEVEN_D_MS} now={now} />
          {a.stale ? <p className="text-[10px] text-neutral-500">Last refresh failed: old numbers</p> : null}
        </div>
      )}
    </article>
  );
}

/** A usage window: the bar with the even-pace mark, the percent, and when it resets. */
function UsageBar({
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
  const pace = now == null ? null : pacePct(win.resetsAt, windowMs, now);
  const reset = now == null || !win.resetsAt ? null : Date.parse(win.resetsAt);
  return (
    <div className="flex items-center gap-1.5">
      <span className="w-7 shrink-0 text-[10px] font-medium text-neutral-500">{label}</span>
      <div className="relative h-1.5 flex-1 overflow-hidden rounded-sm bg-neutral-700">
        <div className={`h-full ${barColor(pct)}`} style={{ width: `${Math.max(1, pct)}%` }} />
        {pace == null ? null : (
          <div className="absolute top-0 h-full w-px bg-neutral-100/70" style={{ left: `${pace}%` }} />
        )}
      </div>
      <span className="w-8 shrink-0 text-right text-[11px] font-medium tabular-nums text-neutral-200">{pct}%</span>
      <span className="w-14 shrink-0 text-right text-[10px] tabular-nums text-neutral-500">
        {reset == null || Number.isNaN(reset) ? "" : reset <= (now ?? 0) ? "reset" : `in ${until(reset - (now ?? 0))}`}
      </span>
    </div>
  );
}

/** How the clone's sound plays here: the buffer's depth now, the gaps (packets that came too
 *  late, each one a short break in the sound) and the packets dropped since the sound started,
 *  and the phone's audio settings. To tell crackles from the network from those of the phone. */
function SoundLine({ r }: { r: SoundReport }) {
  const ms = (s: number) => `${Math.round(s * 1000)} ms`;
  return (
    <p className="px-1 pt-1 text-[11px] tabular-nums text-neutral-500">
      Sound here: buffer {r.target * 10} ms · {r.late} late · {r.dropped} dropped · {r.sampleRate} Hz · output{" "}
      {ms(r.baseLatency + r.outputLatency)}
    </p>
  );
}
