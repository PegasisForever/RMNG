// The Usage tab: every account's usage, in its pools (`groupAccounts`), and the
// jobs the server is running. The reset time of a window is on its bar, as "in 2h 15m", in the
// tab and in the rail alike: a touch screen has no hover.
//
// Pure: the accounts, the clock and the locale come in as props, and the refresh and the
// sign-in go out as callbacks. The server's CPU, memory and disk are in the tool row above, or,
// where it has no room for them, at the top of this tab (`server`).
//
// Wide (`rail`), it is the rail: one column of accounts at the left of the board,
// which scrolls with no bar showing.
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
} from "~/lib/accountUsage";
import { OperationProgress } from "~/components/OperationProgress";
import { type AcctOrder, orderedWithinBuckets } from "~/lib/accountOrder";
import { until } from "~/lib/format";
import type { ClaudeUsage, ClaudeUsageWindow, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { SoundReport } from "~/viewer/protocol";

import type { ServerUsage } from "../ToolRow";
import { BOX, CARD_BOX, DANGER_TEXT, EmptyLine, HeaderButton, LINE, TabPanel, TabHeader, R_PIECE, TEXT, TEXT_2, TEXT_3 } from "./ui";

export interface UsageViewProps {
  /** Both providers' accounts (`ControlState.claudeAccounts`). */
  accounts: ClaudeUsage[];
  /** The operator's order of them, from Settings. */
  accountOrder: AcctOrder;
  /** The pools; with none the list is one flat group. */
  groups: CloneGroup[];
  /** Wall-clock ms for the pace marks and the reset times; null until the clock ticked. */
  now: number | null;
  /** All operations; the running ones show at the top. */
  operations: Operation[];
  /** How the clone's sound plays in this window (the line at the bottom); null before it plays. */
  sound?: SoundReport | null;
  onRefresh: () => void | Promise<void>;
  /** Sign in to an account that takes over from this dead one. */
  onReplaceAccount: (account: ClaudeUsage) => void;
  /** The server's CPU, memory and disk, at the top; absent: the tool row has them. */
  server?: ServerUsage | null;
  /** One column of accounts (the rail, wide), not two. */
  rail?: boolean;
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
  server,
  rail = false,
}: UsageViewProps) {
  const grid = rail ? "grid grid-cols-1 gap-1.5" : "grid grid-cols-[repeat(auto-fill,minmax(230px,1fr))] gap-1.5";
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
    <TabPanel
      noScrollbar={rail}
      header={
        <TabHeader title="Account usage" sub={accounts.length && !rail ? `${accounts.length} accounts` : undefined}>
          {accounts.length ? (
            <HeaderButton icon={RefreshCw} label={busy ? "Refreshing…" : "Refresh"} showLabel={!rail} disabled={busy} onClick={refresh} />
          ) : null}
        </TabHeader>
      }
    >
      {/* The rail's cards sit as the board's columns' do (`CARD_LIST`'s inset) */}
      <div className={`space-y-3 ${rail ? "px-1.5 pb-1.5 pt-1" : "p-2"}`}>
        {server !== undefined ? <ServerStats usage={server} /> : null}
        {running.length ? (
          <section aria-label="Running jobs" className="space-y-1.5">
            <h3 className={`px-1 text-[11px] font-medium ${TEXT_3}`}>Running jobs · {running.length}</h3>
            <div className={grid}>
              {running.map((op) => (
                <OperationProgress key={op.id} op={op} box={CARD_BOX} />
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
                  <h3 className={`flex items-center gap-1.5 px-1 pb-1 text-[11px] font-medium ${TEXT_2}`}>
                    <Folder aria-hidden className={`size-3 ${TEXT_3}`} />
                    {name}
                  </h3>
                ) : null}
                {section.accounts.length === 0 ? (
                  <p className={`px-1 text-xs ${TEXT_3}`}>No accounts</p>
                ) : (
                  <div className={grid}>
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
    </TabPanel>
  );
}

/** One account: who, what it costs, and a bar per usage window with its reset time. */
function AccountCard({
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
  /** Its tokens no longer work: the sign-in button says all of it, so the server's error (the
   *  provider's own words) does not show. */
  const signedOut = a.assignable === false;
  return (
    <article className={`min-w-0 px-2.5 py-2 ${CARD_BOX}`}>
      <div className="flex items-center gap-1.5">
        <img
          src={codex ? chatgptLogo : claudeLogo}
          alt={codex ? "ChatGPT" : "Claude"}
          className={`size-3.5 shrink-0 object-contain ${codex ? "dark:invert" : ""}`}
        />
        <span className={`min-w-0 flex-1 truncate text-xs font-medium ${TEXT}`}>{a.email}</span>
        {a.spend ? <span className={`shrink-0 text-[11px] tabular-nums ${TEXT_2}`}>{spendLine(a.spend)}</span> : null}
        {credits == null ? null : (
          <span className={`shrink-0 text-[11px] tabular-nums ${credits === 0 ? DANGER_TEXT : TEXT_2}`}>
            ⟳ {credits}
          </span>
        )}
      </div>
      {signedOut ? (
        <button
          type="button"
          onClick={() => onReplace(a)}
          className={`mt-1.5 w-full ${R_PIECE} border border-black/15 bg-white/50 py-1.5 text-xs font-medium text-rose-600 hover:bg-white/80 active:bg-white/90 dark:border-white/15 dark:bg-white/5 dark:text-rose-400 dark:hover:bg-white/10 dark:active:bg-white/15`}
        >
          Token expired. Sign in again
        </button>
      ) : null}
      {empty ? (
        signedOut ? null : (
          <p className="mt-1 text-[11px] text-rose-600 dark:text-rose-400">Usage not available{a.error ? `: ${a.error}` : ""}</p>
        )
      ) : (
        <div className={`mt-1.5 space-y-1 ${a.stale ? "opacity-60" : ""}`}>
          <UsageBar label="5h" win={a.fiveHour} windowMs={FIVE_H_MS} now={now} />
          <UsageBar label="7d" win={a.sevenDay} windowMs={SEVEN_D_MS} now={now} />
          <UsageBar label="fable" win={a.fable} windowMs={SEVEN_D_MS} now={now} />
          {a.stale ? <p className={`text-[10px] ${TEXT_3}`}>Last refresh failed: old numbers</p> : null}
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
  const resetText =
    reset == null || Number.isNaN(reset) ? "" : reset <= (now ?? 0) ? "reset" : `in ${until(reset - (now ?? 0))}`;
  return (
    <div className="flex items-center gap-1.5">
      <span className={`w-7 shrink-0 text-[10px] font-medium ${TEXT_3}`}>{label}</span>
      <div className="relative h-1.5 flex-1 overflow-hidden rounded-sm bg-black/10 dark:bg-white/15">
        <div className={`h-full ${barColor(pct)}`} style={{ width: `${Math.max(1, pct)}%` }} />
        {pace == null ? null : (
          <div className="absolute top-0 h-full w-px bg-black/50 dark:bg-white/70" style={{ left: `${pace}%` }} />
        )}
      </div>
      <span className={`w-8 shrink-0 text-right text-[11px] font-medium tabular-nums ${TEXT}`}>{pct}%</span>
      <span className={`w-14 shrink-0 text-right text-[10px] tabular-nums ${TEXT_3}`}>{resetText}</span>
    </div>
  );
}

/** How the clone's sound plays here: the buffer's depth now, the gaps (packets that came too
 *  late, each one a short break in the sound) and the packets dropped since the sound started,
 *  and this device's audio settings. To tell crackles from the network from those of the device. */
function SoundLine({ r }: { r: SoundReport }) {
  const ms = (s: number) => `${Math.round(s * 1000)} ms`;
  return (
    <p className={`px-1 pt-1 text-[11px] tabular-nums ${TEXT_3}`}>
      Sound here: buffer {r.target * 10} ms · {r.late} late · {r.dropped} dropped · {r.sampleRate} Hz · output{" "}
      {ms(r.baseLatency + r.outputLatency)}
    </p>
  );
}

/** The server's CPU, memory and disk, at the top of the rail. */
export function ServerStats({ usage }: { usage: ServerUsage | null }) {
  const stats: [string, string | undefined][] = [
    ["CPU", usage?.cpu],
    ["Memory", usage?.mem],
    ["Disk", usage?.disk],
  ];
  return (
    <div className={`grid grid-cols-3 divide-x ${R_PIECE} border ${LINE} ${BOX} [&>*]:border-black/10 dark:[&>*]:border-white/10`}>
      {stats.map(([label, value]) => (
        <div key={label} className="px-2.5 py-1.5 leading-tight">
          <div className={`text-[10px] ${TEXT_3}`}>{label}</div>
          <div className={`font-mono text-xs font-semibold tabular-nums ${TEXT}`}>{value ?? "—"}</div>
        </div>
      ))}
    </div>
  );
}
