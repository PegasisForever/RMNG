// The board's leftmost column: the controls that belong to the whole rig rather than to
// any one clone. Layout presets, account usage, the CT totals, and whatever operations
// are running. Creating a clone belongs to a column, so that button lives there.
//
// It is the first thing in the strip and scrolls with the columns.
import { Settings } from "lucide-react";
import { useId } from "react";

import { ClaudeAccountsPanel } from "~/components/ClaudeAccountsPanel";
import { OperationProgress } from "~/components/OperationProgress";
import type { AcctOrder } from "~/lib/accountOrder";
import type { ClaudeUsage, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { LxcStats } from "~/lib/wire/LxcStats";

/** The whole-container totals as one line: CPU percentage, memory used, physical disk used.
 *  A metric the host could not sample reads as an em-dash rather than a zero, so an
 *  unavailable figure never passes for an idle one. Null in, null out. */
export function formatLxcUsage(
    stats: LxcStats | null,
): { cpu: string; mem: string; disk: string } | null {
    if (!stats) return null;

    const GiB = 1024 ** 3;
    const cpu =
        stats.cpuPct === null
            ? "—"
            : `${stats.cpuPct < 1 ? stats.cpuPct.toFixed(1) : Math.round(stats.cpuPct)}%`;
    const mem = `${(Number(stats.memUsed) / GiB).toFixed(1)}GB`;
    const disk =
        stats.diskUsed === null
            ? "—"
            : `${(Number(stats.diskUsed) / GiB).toFixed(1)}GB`;
    return { cpu, mem, disk };
}

export interface BoardRailProps {
    /** Per-account usage rows (both providers), from `ControlState.claudeAccounts`. */
    accounts: ClaudeUsage[];
    /** The operator's own ordering of those rows, as dragged out in Settings. The route
     *  subscribes to the shared store and passes the value, so a drag there reorders here. */
    accountOrder: AcctOrder;
    /** The single configured pool list (`config.groups`) — the usage list groups by these. */
    groups?: CloneGroup[];
    /** Formats the usage bars' reset-time tooltips. Read once by the route (the operator's
     *  `navigator.language`) and handed down, so a story can pin it. */
    locale: string;
    /** Wall-clock milliseconds for the usage bars' pace markers and reset tooltips. Read once
     *  by the route, null until its clock has ticked. */
    now: number | null;
    /** Live CT-wide CPU/RAM/rootfs usage (the volatile `lxcStats` SSE event). */
    lxcStats: LxcStats | null;
    /** All operations; the running ones render as progress rows. */
    operations: Operation[];
    /** Layout preset names, in config order. */
    presetNames: string[];
    /** The active preset name. */
    activeLayout: string;
    onActivateLayout: (name: string) => void;
    onOpenSettings: () => void;
    /** Import an account from a clone that is already signed in. */

    /** Sign in to an account that takes over from a dead one (the "sign in again" badge). */
    onReplaceAccount: (account: ClaudeUsage) => void;
    /** Trigger an immediate usage refresh. */
    onRefresh: () => void | Promise<void>;
}

export function BoardRail({
    accounts,
    accountOrder,
    groups,
    locale,
    now,
    lxcStats,
    operations,
    presetNames,
    activeLayout,
    onActivateLayout,
    onOpenSettings,
    onReplaceAccount,
    onRefresh,
}: BoardRailProps) {
    const headingId = useId();
    const jobsHeadingId = useId();
    const lxcUsage = formatLxcUsage(lxcStats);
    const running = operations.filter((o) => o.status === "running");

    return (
        <aside
            aria-labelledby={headingId}
            className="flex w-64 shrink-0 flex-col gap-2 overflow-y-auto px-1 pb-2 pt-1"
        >
            <div className="space-y-2 pb-2">
                <div className="flex items-center justify-between">
                    <h2
                        id={headingId}
                        className="text-sm font-semibold text-slate-800 dark:text-slate-100"
                    >
                        RMNG Control
                    </h2>
                    <button
                        type="button"
                        onClick={onOpenSettings}
                        title="Settings"
                        aria-label="Settings"
                        className="rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600 dark:text-slate-500 dark:hover:bg-slate-800 dark:hover:text-slate-300"
                    >
                        <Settings className="size-4" />
                    </button>
                </div>

                {presetNames.length > 0 ? (
                    <div className="flex items-center gap-2">
                        <h3 className="shrink-0 text-[11px] font-medium text-slate-500 dark:text-slate-400">
                            Layout
                        </h3>
                        <div
                            aria-label="Layout"
                            role="group"
                            className="flex flex-wrap gap-1"
                        >
                            {presetNames.map((name) => {
                                const active = name === activeLayout;
                                return (
                                    <button
                                        key={name}
                                        type="button"
                                        onClick={() => onActivateLayout(name)}
                                        aria-pressed={active}
                                        className={`rounded-md border px-2 py-0.5 text-xs font-medium ${
                                            active
                                                ? "border-emerald-600 bg-emerald-600 text-white"
                                                : "border-slate-200 bg-white text-slate-600 hover:bg-slate-100 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300 dark:hover:bg-slate-800"
                                        }`}
                                    >
                                        {name}
                                    </button>
                                );
                            })}
                        </div>
                    </div>
                ) : null}

                {lxcUsage ? (
                    <dl
                        aria-label="Server usage"
                        className="grid grid-cols-3 divide-x divide-slate-200 rounded-md bg-slate-100 py-1 tabular-nums dark:divide-slate-700 dark:bg-slate-800/60"
                        title="CT LXC totals: CPU and memory include all LXC processes; memory is RAM + swap excluding reclaimable file cache; disk is physical, compression-aware ZFS rootfs use"
                    >
                        {[
                            ["CPU", lxcUsage.cpu],
                            ["Memory", lxcUsage.mem],
                            ["Disk", lxcUsage.disk],
                        ].map(([label, value]) => (
                            <div key={label} className="px-2">
                                <dt className="text-[10px] text-slate-500 dark:text-slate-400">
                                    {label}
                                </dt>
                                <dd className="text-[11px] font-semibold text-slate-700 dark:text-slate-200">
                                    {value}
                                </dd>
                            </div>
                        ))}
                    </dl>
                ) : null}
            </div>

            <ClaudeAccountsPanel
                accounts={accounts}
                accountOrder={accountOrder}
                groups={groups}
                locale={locale}
                now={now}
                onReplace={onReplaceAccount}
                onRefresh={onRefresh}
            />

            {running.length > 0 ? (
                <section
                    aria-labelledby={jobsHeadingId}
                    className="space-y-1.5 pt-2"
                >
                    <h2
                        id={jobsHeadingId}
                        className="flex items-center gap-2 text-xs font-semibold text-slate-700 dark:text-slate-200"
                    >
                        Active jobs
                        <span className="rounded bg-sky-100 px-1.5 py-0.5 text-[10px] tabular-nums text-sky-700 dark:bg-sky-950 dark:text-sky-300">
                            {running.length}
                        </span>
                    </h2>
                    {[...running]
                        .sort((a, b) => b.startedAt - a.startedAt)
                        .map((op) => (
                            <OperationProgress key={op.id} op={op} />
                        ))}
                </section>
            ) : null}
        </aside>
    );
}
