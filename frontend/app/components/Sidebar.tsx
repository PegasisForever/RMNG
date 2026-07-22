import {
  closestCenter,
  DndContext,
  type DragEndEvent,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  arrayMove,
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { Settings } from "lucide-react";

import { ClaudeAccountsPanel } from "~/components/ClaudeAccountsPanel";
import { OperationProgress } from "~/components/OperationProgress";
import { SidebarHost } from "~/components/SidebarHost";
import { formatBytes } from "~/lib/format";
import type { GroupUsage, Host, Operation } from "~/lib/types";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";

export function formatHostsUsageSummary(
  hostIds: string[],
  stats: Record<string, ContainerStats>,
  cloneCpus: number,
): { cpu: string; mem: string; disk?: string } | null {
  let cpuPctTotal = 0;
  let memUsedTotal = 0;
  let dockerDiskUsed = 0;
  let sampleCount = 0;

  for (const id of hostIds) {
    const sample = stats[id];
    if (!sample || Number(sample.memLimit) <= 0) continue;
    cpuPctTotal += sample.cpuPct;
    memUsedTotal += Number(sample.memUsed);
    dockerDiskUsed = Math.max(dockerDiskUsed, Number(sample.dockerDiskUsed ?? 0));
    sampleCount += 1;
  }

  if (sampleCount === 0) return null;

  const GiB = 1024 ** 3;
  const mem = `${(memUsedTotal / GiB).toFixed(1)}GB`;
  const cpu =
    cloneCpus > 0
      ? (() => {
          const pct = cpuPctTotal / cloneCpus;
          return `${pct < 1 ? pct.toFixed(1) : Math.round(pct)}%`;
        })()
      : `${(cpuPctTotal / 100).toFixed(1)}c`;
  return {
    cpu,
    mem,
    ...(dockerDiskUsed > 0 ? { disk: formatBytes(dockerDiskUsed).replace(" ", "") } : {}),
  };
}

export function partitionHosts(hosts: Host[]): { activeHosts: Host[]; archivedHosts: Host[] } {
  return {
    activeHosts: hosts.filter((host) => !host.archived),
    archivedHosts: hosts.filter((host) => host.archived),
  };
}

/** Merge a reordered active-host projection back into the complete persisted order without
 * moving archived rows. This keeps an archived clone's place when it is restored. */
export function mergeActiveHostOrder(hosts: Host[], activeOrder: string[]): string[] {
  const nextActive = [...activeOrder];
  return hosts.map((host) => (host.archived ? host.id : (nextActive.shift() ?? host.id)));
}

export interface SidebarProps {
  /** Off-canvas drawer state (< lg); the panel is static + always visible ≥ lg. */
  open?: boolean;
  /** Account pools + their usage (configured groups merged with `ControlState.usageGroups`). */
  usageGroups: GroupUsage[];
  /** Hosts in display order — already reconciled + reordered by the container. */
  hosts: Host[];
  /** Live per-host CPU/RAM map (the volatile `stats` SSE event). */
  stats: Record<string, ContainerStats>;
  /** Live per-host forward-runtime map (the `forwards` SSE event), fanned out to each
   *  host row's compact forwards chips. */
  forwards?: Record<string, ForwardRuntime[]>;
  /** All operations; the sidebar derives per-host badges, the clone-busy state,
   *  and the Activity list from these. */
  operations: Operation[];
  selectedId: string | null;
  /** `docker.cloneCpus` — normalizes each host row's CPU usage figure. */
  cloneCpus: number;
  /** `ssh.publicHost` (config) — the `-J` jump target for each row's copied SSH
   *  command. Empty ⇒ each row falls back to `window.location.hostname`. */
  sshPublicHost: string;
  /** `listen.bastion` — the bastion `sshd` port each row's copied SSH command jumps
   *  through. */
  bastionPort: number;
  /** Layout preset names (config order) — the segmented switcher buttons. */
  presetNames: string[];
  /** The active preset name (highlighted). */
  activeLayout: string;
  /** Activate a layout preset (live-applies to all running clones). */
  onActivateLayout: (name: string) => void;

  onOpenSettings: () => void;
  onOpenClone: () => void;
  /** Create a new account group. */
  onCreateGroup: () => void;
  /** Add an account to a group (opens the OAuth login flow). */
  onAddAccount: (group: string) => void;
  /** Delete an account group. */
  onDeleteGroup: (group: string) => void;
  /** Trigger an immediate usage refresh (the panel's refresh button). */
  onRefresh: () => void | Promise<void>;
  onSelectHost: (host: Host) => void;
  onDeleteHost: (host: Host) => void;
  /** Commit a managed clone to a new clone-source image. */
  onCommitHost: (host: Host) => void;
  /** Change a managed clone's account-group binding. */
  onChangeAccountHost: (host: Host) => void;
  /** Open the port-forward editor for a host. */
  onPortForwardHost: (host: Host) => void;
  /** Gracefully stop a managed clone while retaining it. */
  onArchiveHost: (host: Host) => void;
  /** Restart a retained managed clone. */
  onUnarchiveHost: (host: Host) => void;
  /** New host id order after a drag-reorder. */
  onReorder: (nextIds: string[]) => void;
}

/** The left host-selection panel: account groups, the drag-reorderable host list,
 *  and running-operation progress. Purely presentational — every server interaction
 *  is a prop callback, so it renders standalone (e.g. in Storybook) with mocked data.
 *  Off-canvas drawer < lg, static ≥ lg. */
export function Sidebar({
  open = false,
  usageGroups,
  hosts,
  stats,
  forwards = {},
  operations,
  selectedId,
  cloneCpus,
  sshPublicHost,
  bastionPort,
  presetNames,
  activeLayout,
  onActivateLayout,
  onOpenSettings,
  onOpenClone,
  onCreateGroup,
  onAddAccount,
  onDeleteGroup,
  onRefresh,
  onSelectHost,
  onDeleteHost,
  onCommitHost,
  onChangeAccountHost,
  onPortForwardHost,
  onArchiveHost,
  onUnarchiveHost,
  onReorder,
}: SidebarProps) {
  const runningClone = operations.some(
    (o) => o.kind === "clone" && o.status === "running",
  );
  const opForHost = (id: string) =>
    operations.find((o) => o.target === id && o.status === "running");
  const { activeHosts, archivedHosts } = partitionHosts(hosts);
  const hostsUsage = formatHostsUsageSummary(
    activeHosts.map((h) => h.id),
    stats,
    cloneCpus,
  );

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const ids = activeHosts.map((h) => h.id);
    const oldIndex = ids.indexOf(String(active.id));
    const newIndex = ids.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    onReorder(mergeActiveHostOrder(hosts, arrayMove(ids, oldIndex, newIndex)));
  }

  return (
    <aside
      className={`fixed inset-y-0 left-0 z-40 flex w-96 max-w-[90vw] shrink-0 flex-col gap-3 overflow-y-auto border-r border-slate-200 bg-slate-50 p-3 shadow-xl transition-transform duration-200 lg:static lg:z-auto lg:translate-x-0 lg:shadow-none dark:border-slate-700 dark:bg-slate-900 ${
        open ? "translate-x-0" : "-translate-x-full"
      }`}
    >
      <div className="flex items-center justify-between px-1">
        <span className="text-xs font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
          rmng control
        </span>
        <button
          type="button"
          onClick={onOpenSettings}
          title="Settings"
          aria-label="Settings"
          className="rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-600 dark:text-slate-500 dark:hover:bg-slate-700 dark:hover:text-slate-300"
        >
          <Settings className="size-4" />
        </button>
      </div>

      {presetNames.length > 0 ? (
        <div className="px-1">
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

      <ClaudeAccountsPanel
        groups={usageGroups}
        onCreateGroup={onCreateGroup}
        onAddAccount={onAddAccount}
        onDeleteGroup={onDeleteGroup}
        onRefresh={onRefresh}
      />

      <div>
        <div className="mb-1 flex items-center justify-between px-1">
          <div className="flex min-w-0 items-baseline gap-2">
            <h2 className="shrink-0 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
              Hosts ({activeHosts.length})
            </h2>
            {hostsUsage ? (
              <span
                className="truncate text-[11px] font-semibold tabular-nums text-slate-500 dark:text-slate-400"
                title="Total live container CPU and memory usage; Docker daemon disk usage"
              >
                CPU {hostsUsage.cpu} · MEM {hostsUsage.mem}
                {hostsUsage.disk ? <> · DISK {hostsUsage.disk}</> : null}
              </span>
            ) : null}
          </div>
          <button
            type="button"
            onClick={onOpenClone}
            disabled={runningClone}
            title="Create a new clone from a source image"
            className="rounded px-1 text-[11px] font-medium text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-40 dark:text-slate-500 dark:hover:bg-slate-700 dark:hover:text-slate-300"
          >
            + Clone
          </button>
        </div>
        {activeHosts.length === 0 ? (
          <p className="rounded-lg border border-dashed border-slate-300 bg-white p-4 text-center text-xs text-slate-400 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-500">
            {archivedHosts.length === 0 ? "No hosts yet." : "No active hosts."}
          </p>
        ) : (
          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            onDragEnd={onDragEnd}
          >
            <SortableContext
              items={activeHosts.map((h) => h.id)}
              strategy={verticalListSortingStrategy}
            >
              <div>
                {activeHosts.map((host) => (
                  <SidebarHost
                    key={host.id}
                    host={host}
                    stats={stats[host.id]}
                    forwardRuntime={forwards[host.id]}
                    cloneCpus={cloneCpus}
                    sshPublicHost={sshPublicHost}
                    bastionPort={bastionPort}
                    selected={selectedId === host.id}
                    op={opForHost(host.id)}
                    onSelect={() => onSelectHost(host)}
                    onCommit={() => onCommitHost(host)}
                    onDelete={() => onDeleteHost(host)}
                    onChangeAccount={() => onChangeAccountHost(host)}
                    onPortForward={() => onPortForwardHost(host)}
                    onArchive={() => onArchiveHost(host)}
                    onUnarchive={() => onUnarchiveHost(host)}
                  />
                ))}
              </div>
            </SortableContext>
          </DndContext>
        )}
      </div>

      {archivedHosts.length > 0 ? (
        <div>
          <h2 className="mb-1 px-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
            Archived hosts ({archivedHosts.length})
          </h2>
          <DndContext>
            <SortableContext items={[]} strategy={verticalListSortingStrategy}>
              <div>
                {archivedHosts.map((host) => (
                  <SidebarHost
                    key={host.id}
                    host={host}
                    cloneCpus={cloneCpus}
                    sshPublicHost={sshPublicHost}
                    bastionPort={bastionPort}
                    selected={selectedId === host.id}
                    op={opForHost(host.id)}
                    onSelect={() => onSelectHost(host)}
                    onCommit={() => onCommitHost(host)}
                    onDelete={() => onDeleteHost(host)}
                    onChangeAccount={() => onChangeAccountHost(host)}
                    onPortForward={() => onPortForwardHost(host)}
                    onArchive={() => onArchiveHost(host)}
                    onUnarchive={() => onUnarchiveHost(host)}
                  />
                ))}
              </div>
            </SortableContext>
          </DndContext>
        </div>
      ) : null}

      {operations.length > 0 ? (
        <div className="space-y-2">
          <h2 className="px-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
            Activity
          </h2>
          {[...operations]
            .sort((a, b) => b.startedAt - a.startedAt)
            .map((op) => (
              <OperationProgress key={op.id} op={op} />
            ))}
        </div>
      ) : null}
    </aside>
  );
}
