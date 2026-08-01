// The board's leftmost column: the controls that belong to the whole rig rather than to
// any one clone. Layout presets, account usage, the CT totals, and whatever operations
// are running. Creating a clone belongs to a column, so that button lives there.
//
// It is the first thing in the strip and scrolls with the columns.
import { Settings } from "lucide-react";

import { ClaudeAccountsPanel } from "~/components/ClaudeAccountsPanel";
import { OperationProgress } from "~/components/OperationProgress";
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
  const cpu = stats.cpuPct === null
    ? "—"
    : `${stats.cpuPct < 1 ? stats.cpuPct.toFixed(1) : Math.round(stats.cpuPct)}%`;
  const mem = `${(Number(stats.memUsed) / GiB).toFixed(1)}GB`;
  const disk = stats.diskUsed === null ? "—" : `${(Number(stats.diskUsed) / GiB).toFixed(1)}GB`;
  return { cpu, mem, disk };
}

export interface BoardRailProps {
  /** Per-account usage rows (both providers), from `ControlState.claudeAccounts`. */
  accounts: ClaudeUsage[];
  /** Configured Claude pools (`config.cloneGroups`) — the usage list groups by these. */
  cloneGroups?: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups?: CloneGroup[];
  /** Formats the usage bars' reset-time tooltips. Read once by the route (the operator's
   *  `navigator.language`) and handed down, so a story can pin it. */
  locale: string;
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
  onImportAccount: () => void;
  /** Trigger an immediate usage refresh. */
  onRefresh: () => void | Promise<void>;
}

export function BoardRail({
  accounts,
  cloneGroups,
  codexGroups,
  locale,
  lxcStats,
  operations,
  presetNames,
  activeLayout,
  onActivateLayout,
  onOpenSettings,
  onImportAccount,
  onRefresh,
}: BoardRailProps) {
  const lxcUsage = formatLxcUsage(lxcStats);
  const running = operations.filter((o) => o.status === "running");

  return (
    <aside className="flex w-64 shrink-0 flex-col gap-3 overflow-y-auto px-1 pt-1">
      <div className="flex items-center justify-between">
        <span className="text-xs font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
          rmng control
        </span>
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
        <div>
          <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
            Layout
          </div>
          <div className="flex flex-wrap gap-1">
            {presetNames.map((name) => {
              const active = name === activeLayout;
              return (
                <button
                  key={name}
                  type="button"
                  onClick={() => onActivateLayout(name)}
                  aria-pressed={active}
                  className={`rounded px-2 py-1 text-xs font-medium ${
                    active
                      ? "bg-emerald-600 text-white"
                      : "border border-slate-300 text-slate-600 hover:bg-slate-100 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-800"
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
        <div
          className="px-1 text-[11px] font-semibold tabular-nums text-slate-500 dark:text-slate-400"
          title="CT LXC totals: CPU and memory include all LXC processes; memory is RAM + swap excluding reclaimable file cache; disk is physical, compression-aware ZFS rootfs use"
        >
          CPU {lxcUsage.cpu} · MEM {lxcUsage.mem} · DISK {lxcUsage.disk}
        </div>
      ) : null}

      <ClaudeAccountsPanel
        accounts={accounts}
        cloneGroups={cloneGroups}
        codexGroups={codexGroups}
        locale={locale}
        onImport={onImportAccount}
        onRefresh={onRefresh}
      />

      {running.length > 0 ? (
        <div className="space-y-2">
          <h2 className="text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
            Activity
          </h2>
          {[...running]
            .sort((a, b) => b.startedAt - a.startedAt)
            .map((op) => (
              <OperationProgress key={op.id} op={op} />
            ))}
        </div>
      ) : null}
    </aside>
  );
}
