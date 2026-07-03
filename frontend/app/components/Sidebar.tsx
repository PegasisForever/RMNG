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

import { ClaudeAccountsPanel } from "~/components/ClaudeAccountsPanel";
import { OperationProgress } from "~/components/OperationProgress";
import { SidebarHost } from "~/components/SidebarHost";
import type { ClaudeUsage, Host, Operation } from "~/lib/types";
import type { ContainerStats } from "~/lib/wire/ContainerStats";

export interface SidebarProps {
  /** Off-canvas drawer state (< lg); the panel is static + always visible ≥ lg. */
  open?: boolean;
  /** Claude accounts usage list (from `ControlState.claudeAccounts`). */
  accounts: ClaudeUsage[];
  /** Hosts in display order — already reconciled + reordered by the container. */
  hosts: Host[];
  /** Live per-host CPU/RAM map (the volatile `stats` SSE event). */
  stats: Record<string, ContainerStats>;
  /** All operations; the sidebar derives per-host badges, the clone-busy state,
   *  and the Activity list from these. */
  operations: Operation[];
  selectedId: string | null;
  /** `docker.cloneCpus` — normalizes each host row's CPU usage figure. */
  cloneCpus: number;

  onOpenSettings: () => void;
  onOpenClone: () => void;
  onRefreshClaude: () => void;
  onImportAccount: () => void;
  onSelectHost: (host: Host) => void;
  onDeleteHost: (host: Host) => void;
  /** Commit a managed clone to a new clone-source image. */
  onCommitHost: (host: Host) => void;
  /** Change a managed clone's Claude account / group. */
  onChangeAccountHost: (host: Host) => void;
  /** New host id order after a drag-reorder. */
  onReorder: (nextIds: string[]) => void;
}

function SettingsIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 20 20"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      aria-hidden
    >
      <circle cx="10" cy="10" r="3" />
      <path d="M10 1.5v2M10 16.5v2M18.5 10h-2M3.5 10h-2M15.6 4.4l-1.4 1.4M5.8 14.2l-1.4 1.4M15.6 15.6l-1.4-1.4M5.8 5.8 4.4 4.4" />
    </svg>
  );
}

/** The left host-selection panel: Claude accounts, the drag-reorderable host list,
 *  and running-operation progress. Purely presentational — every server interaction
 *  is a prop callback, so it renders standalone (e.g. in Storybook) with mocked data.
 *  Off-canvas drawer < lg, static ≥ lg. */
export function Sidebar({
  open = false,
  accounts,
  hosts,
  stats,
  operations,
  selectedId,
  cloneCpus,
  onOpenSettings,
  onOpenClone,
  onRefreshClaude,
  onImportAccount,
  onSelectHost,
  onDeleteHost,
  onCommitHost,
  onChangeAccountHost,
  onReorder,
}: SidebarProps) {
  const runningClone = operations.some(
    (o) => o.kind === "clone" && o.status === "running",
  );
  const opForHost = (id: string) =>
    operations.find((o) => o.target === id && o.status === "running");

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const ids = hosts.map((h) => h.id);
    const oldIndex = ids.indexOf(String(active.id));
    const newIndex = ids.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    onReorder(arrayMove(ids, oldIndex, newIndex));
  }

  return (
    <aside
      className={`fixed inset-y-0 left-0 z-40 flex w-96 max-w-[90vw] shrink-0 flex-col gap-3 overflow-y-auto border-r border-slate-200 bg-slate-50 p-3 shadow-xl transition-transform duration-200 lg:static lg:z-auto lg:translate-x-0 lg:shadow-none ${
        open ? "translate-x-0" : "-translate-x-full"
      }`}
    >
      <div className="flex items-center justify-between px-1">
        <span className="text-xs font-semibold uppercase tracking-wide text-slate-400">
          rmng control
        </span>
        <button
          type="button"
          onClick={onOpenSettings}
          title="Settings"
          aria-label="Settings"
          className="rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-600"
        >
          <SettingsIcon />
        </button>
      </div>

      <ClaudeAccountsPanel
        accounts={accounts}
        onRefresh={onRefreshClaude}
        onImport={onImportAccount}
      />

      <div>
        <div className="mb-1 flex items-center justify-between px-1">
          <h2 className="text-[11px] font-semibold uppercase tracking-wide text-slate-400">
            Hosts ({hosts.length})
          </h2>
          <button
            type="button"
            onClick={onOpenClone}
            disabled={runningClone}
            title="Create a new clone from a source image"
            className="rounded px-1 text-[11px] font-medium text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-40"
          >
            + Clone
          </button>
        </div>
        {hosts.length === 0 ? (
          <p className="rounded-lg border border-dashed border-slate-300 bg-white p-4 text-center text-xs text-slate-400">
            No hosts yet.
          </p>
        ) : (
          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            onDragEnd={onDragEnd}
          >
            <SortableContext
              items={hosts.map((h) => h.id)}
              strategy={verticalListSortingStrategy}
            >
              <div>
                {hosts.map((host) => (
                  <SidebarHost
                    key={host.id}
                    host={host}
                    stats={stats[host.id]}
                    cloneCpus={cloneCpus}
                    selected={selectedId === host.id}
                    op={opForHost(host.id)}
                    onSelect={() => onSelectHost(host)}
                    onCommit={() => onCommitHost(host)}
                    onDelete={() => onDeleteHost(host)}
                    onChangeAccount={() => onChangeAccountHost(host)}
                  />
                ))}
              </div>
            </SortableContext>
          </DndContext>
        )}
      </div>

      {operations.length > 0 ? (
        <div className="space-y-2">
          <h2 className="px-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400">
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
