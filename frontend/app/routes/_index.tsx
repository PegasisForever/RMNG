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
import { lazy, Suspense, useEffect, useState } from "react";

import { ChangeAccountModal } from "~/components/ChangeAccountModal";
import { ClaudeAccountsPanel } from "~/components/ClaudeAccountsPanel";
import { CloneModal } from "~/components/CloneModal";
import { ImportAccountModal } from "~/components/ImportAccountModal";
import { NewTemplateModal } from "~/components/NewTemplateModal";
import { OperationProgress } from "~/components/OperationProgress";
import { SettingsPanel } from "~/components/SettingsPanel";
import { SidebarHost } from "~/components/SidebarHost";
import {
  activate,
  bootstrapTemplate,
  cloneHost,
  deleteHost,
  redeployClone,
  refreshClaudeUsage,
  reorder,
  swapClaudeAccount,
} from "~/lib/api";
import { type ControlState, type Host, emptyState } from "~/lib/types";

import type { Route } from "./+types/_index";

// BlockNote + the chat panel are browser-only; load them lazily and render only
// after mount so they never participate in SSR.
const HostEditor = lazy(() => import("~/components/HostEditor"));
const ChatPanel = lazy(() => import("~/components/ChatPanel"));

function ClientOnly({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  return mounted ? <>{children}</> : null;
}

export function meta() {
  return [{ title: "rmng control" }];
}

// SPA mode (ssr:false): the live EventSource("/events") delivers the initial full
// state on connect, so the loader just seeds an empty state client-side.
export function clientLoader() {
  return emptyState();
}

/** Initial state from the SSR loader, kept live by the SSE stream. */
function useLiveState(initial: ControlState) {
  const [state, setState] = useState(initial);
  useEffect(() => {
    const es = new EventSource("/events");
    es.onmessage = (e) => {
      try {
        setState(JSON.parse(e.data));
      } catch {
        // ignore malformed frame
      }
    };
    return () => es.close();
  }, []);
  return state;
}

export default function Dashboard({ loaderData }: Route.ComponentProps) {
  const state = useLiveState(loaderData);
  const [error, setError] = useState<string | null>(null);
  const [cloneSource, setCloneSource] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [newTemplateOpen, setNewTemplateOpen] = useState(false);
  const [bootstrapping, setBootstrapping] = useState(false);
  const [changeHost, setChangeHost] = useState<Host | null>(null);
  const [changing, setChanging] = useState(false);
  // Responsive state. Below `lg` the sidebar is an off-canvas drawer; below `xl`
  // the notes editor and agent chat share the main pane via this tab toggle.
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [pane, setPane] = useState<"notes" | "chat">("notes");

  // Local display order for smooth drag-and-drop, reconciled with the server.
  const [order, setOrder] = useState<string[]>(() => state.hosts.map((h) => h.id));
  useEffect(() => {
    const serverIds = state.hosts.map((h) => h.id);
    setOrder((prev) => {
      const sameSet =
        prev.length === serverIds.length && prev.every((id) => serverIds.includes(id));
      return sameSet ? prev : serverIds;
    });
  }, [state.hosts]);

  const hostsById = new Map(state.hosts.map((h) => [h.id, h]));
  const orderedHosts = order.flatMap((id) => {
    const h = hostsById.get(id);
    return h ? [h] : [];
  });
  const selectedHost = state.selected ? hostsById.get(state.selected) ?? null : null;
  const templates = new Set(state.templates);

  const run = (p: Promise<unknown>) =>
    p.then(() => setError(null)).catch((e: Error) => setError(e.message));

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = order.indexOf(String(active.id));
    const newIndex = order.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(order, oldIndex, newIndex);
    setOrder(next); // optimistic; SSE confirms after the server persists
    run(reorder(next));
  }

  const runningClone = state.operations.some(
    (o) => o.kind === "clone" && o.status === "running",
  );
  const opForHost = (id: string) =>
    state.operations.find((o) => o.target === id && o.status === "running");

  return (
    <div className="flex h-screen flex-col">
      {error ? (
        <div className="shrink-0 border-b border-red-200 bg-red-50 px-4 py-2 text-sm text-red-700">
          {error}
        </div>
      ) : null}

      {/* Mobile top bar: hamburger + context. Hidden once the sidebar is static. */}
      <div className="flex shrink-0 items-center gap-2 border-b border-slate-200 bg-white px-3 py-2 lg:hidden">
        <button
          type="button"
          onClick={() => setSidebarOpen(true)}
          aria-label="Open menu"
          className="rounded-md p-1.5 text-slate-600 hover:bg-slate-100"
        >
          <svg
            width="20"
            height="20"
            viewBox="0 0 20 20"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            aria-hidden
          >
            <path d="M3 5h14M3 10h14M3 15h14" />
          </svg>
        </button>
        <span className="min-w-0 flex-1 truncate text-sm font-semibold text-slate-800">
          {selectedHost ? selectedHost.id : "rmng control"}
        </span>
        {/* Notes/Chat toggle lives here on mobile — the only header < lg. */}
        {selectedHost ? (
          <div className="flex shrink-0 gap-0.5 rounded-md bg-slate-100 p-0.5 text-xs font-medium">
            <button
              type="button"
              onClick={() => setPane("notes")}
              className={`rounded px-3 py-1 ${
                pane === "notes"
                  ? "bg-white text-slate-900 shadow-sm"
                  : "text-slate-500 hover:text-slate-700"
              }`}
            >
              Notes
            </button>
            <button
              type="button"
              onClick={() => setPane("chat")}
              className={`rounded px-3 py-1 ${
                pane === "chat"
                  ? "bg-white text-slate-900 shadow-sm"
                  : "text-slate-500 hover:text-slate-700"
              }`}
            >
              Chat
            </button>
          </div>
        ) : null}
      </div>

      <div className="relative flex min-h-0 flex-1">
        {/* Backdrop behind the drawer (mobile only, when open). */}
        {sidebarOpen ? (
          <div
            className="fixed inset-0 z-30 bg-slate-900/40 lg:hidden"
            onClick={() => setSidebarOpen(false)}
            aria-hidden
          />
        ) : null}

        {/* Left: host selection sidebar. Off-canvas drawer < lg, static ≥ lg. */}
        <aside
          className={`fixed inset-y-0 left-0 z-40 flex w-72 shrink-0 flex-col gap-3 overflow-y-auto border-r border-slate-200 bg-slate-50 p-3 shadow-xl transition-transform duration-200 lg:static lg:z-auto lg:translate-x-0 lg:shadow-none ${
            sidebarOpen ? "translate-x-0" : "-translate-x-full"
          }`}
        >
          <div className="flex items-center justify-between px-1">
            <span className="text-xs font-semibold uppercase tracking-wide text-slate-400">
              rmng control
            </span>
            <button
              type="button"
              onClick={() => setSettingsOpen(true)}
              title="Settings"
              aria-label="Settings"
              className="rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-600"
            >
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
            </button>
          </div>

          <ClaudeAccountsPanel
            accounts={state.claudeAccounts ?? []}
            onRefresh={() => run(refreshClaudeUsage())}
            onImport={() => setImportOpen(true)}
          />

          <div>
            <div className="mb-1 flex items-center justify-between px-1">
              <h2 className="text-[11px] font-semibold uppercase tracking-wide text-slate-400">
                Hosts ({orderedHosts.length})
              </h2>
              <button
                type="button"
                onClick={() => setNewTemplateOpen(true)}
                title="Provision a new template container (Ubuntu 26.04)"
                className="rounded px-1 text-[11px] font-medium text-slate-400 hover:bg-slate-200 hover:text-slate-600"
              >
                + Template
              </button>
            </div>
            {orderedHosts.length === 0 ? (
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
                  items={orderedHosts.map((h) => h.id)}
                  strategy={verticalListSortingStrategy}
                >
                  <div className="space-y-0.5">
                    {orderedHosts.map((host) => (
                      <SidebarHost
                        key={host.id}
                        host={host}
                        selected={state.selected === host.id}
                        op={opForHost(host.id)}
                        isTemplate={templates.has(host.id)}
                        cloneBusy={runningClone}
                        onSelect={() => {
                          run(activate(host.id));
                          setSidebarOpen(false);
                        }}
                        onClone={() => setCloneSource(host.id)}
                        onDelete={() => {
                          if (confirm(`Delete ${host.id}? This destroys its container.`))
                            run(deleteHost(host.id));
                        }}
                        onRedeploy={() => {
                          if (
                            confirm(
                              `Redeploy clone-daemon + agent-wrapper to ${host.id}?\n\nSwaps the binaries from the control-server's embedded copies and restarts both units (drops the agent's current Claude session). No reprovision.`,
                            )
                          )
                            run(redeployClone(host.id));
                        }}
                        onChangeAccount={() => setChangeHost(host)}
                      />
                    ))}
                  </div>
                </SortableContext>
              </DndContext>
            )}
          </div>

          {state.operations.length > 0 ? (
            <div className="space-y-2">
              <h2 className="px-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400">
                Activity
              </h2>
              {[...state.operations]
                .sort((a, b) => b.startedAt - a.startedAt)
                .map((op) => (
                  <OperationProgress key={op.id} op={op} />
                ))}
            </div>
          ) : null}
        </aside>

        {/* Right: per-host editor */}
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden bg-white">
          {selectedHost ? (
            <>
              {/* Per-host header — only ≥ lg; on mobile the top bar shows id + tabs. */}
              <div className="hidden shrink-0 items-center gap-3 border-b border-slate-100 px-4 py-3 sm:px-6 lg:flex">
                <h2 className="truncate text-base font-semibold text-slate-900">
                  {selectedHost.id}
                </h2>
                <span className="shrink-0 text-xs text-slate-400">
                  {selectedHost.host}:{selectedHost.port}
                  {selectedHost.ctid != null ? ` · ct ${selectedHost.ctid}` : ""}
                </span>
              </div>
              <div className="flex min-h-0 flex-1">
                {/* Notes editor — full width < lg when its tab is active, else beside chat. */}
                <div
                  className={`min-w-0 flex-1 overflow-y-auto py-4 lg:block lg:border-r lg:border-slate-200 ${
                    pane === "notes" ? "block" : "hidden"
                  }`}
                >
                  <ClientOnly>
                    <Suspense
                      fallback={
                        <div className="p-6 text-sm text-slate-400">Loading editor…</div>
                      }
                    >
                      <HostEditor key={selectedHost.id} hostId={selectedHost.id} />
                    </Suspense>
                  </ClientOnly>
                </div>
                {/* Agent chat — full width < lg when its tab is active, else a fixed column
                    (a touch narrower at the tight lg width so the editor keeps room). */}
                <div
                  className={`w-full shrink-0 flex-col overflow-hidden bg-slate-50/50 lg:flex lg:w-80 xl:w-[400px] ${
                    pane === "chat" ? "flex" : "hidden"
                  }`}
                >
                  <ClientOnly>
                    <Suspense
                      fallback={
                        <div className="p-4 text-sm text-slate-400">Loading chat…</div>
                      }
                    >
                      <ChatPanel key={selectedHost.id} hostId={selectedHost.id} />
                    </Suspense>
                  </ClientOnly>
                </div>
              </div>
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center px-6 text-center text-sm text-slate-400">
              Select a host to open its notes.
            </div>
          )}
        </main>
      </div>

      {cloneSource ? (
        <CloneModal
          source={cloneSource}
          busy={runningClone}
          accounts={(state.claudeAccounts ?? []).filter(
            (a) => a.assignable && a.provider !== "codex",
          )}
          onClose={() => setCloneSource(null)}
          onClone={(payload) => {
            run(cloneHost(cloneSource, payload));
            setCloneSource(null);
          }}
        />
      ) : null}

      {settingsOpen ? (
        <SettingsPanel
          accountEmails={(state.claudeAccounts ?? [])
            .filter((a) => a.provider !== "codex")
            .map((a) => a.email)}
          onClose={() => setSettingsOpen(false)}
        />
      ) : null}

      {newTemplateOpen ? (
        <NewTemplateModal
          busy={bootstrapping}
          existing={new Set(state.hosts.map((h) => h.id))}
          onClose={() => setNewTemplateOpen(false)}
          onCreate={(hostname, resources) => {
            setBootstrapping(true);
            bootstrapTemplate(hostname, resources)
              .then(() => setError(null))
              .catch((e: Error) => setError(e.message))
              .finally(() => {
                setBootstrapping(false);
                setNewTemplateOpen(false);
              });
          }}
        />
      ) : null}

      {importOpen ? (
        <ImportAccountModal
          hosts={state.hosts}
          onClose={() => setImportOpen(false)}
          onImported={() => {
            setImportOpen(false);
            run(refreshClaudeUsage());
          }}
        />
      ) : null}

      {changeHost ? (
        <ChangeAccountModal
          host={changeHost}
          accounts={(state.claudeAccounts ?? []).filter(
            (a) => a.assignable && a.provider !== "codex",
          )}
          busy={changing}
          onClose={() => setChangeHost(null)}
          onSubmit={(value) => {
            setChanging(true);
            swapClaudeAccount(changeHost.id, value)
              .then(() => setError(null))
              .catch((e: Error) => setError(e.message))
              .finally(() => {
                setChanging(false);
                setChangeHost(null);
              });
          }}
        />
      ) : null}
    </div>
  );
}
