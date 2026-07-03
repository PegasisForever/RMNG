import { lazy, Suspense, useEffect, useState } from "react";

import { ChangeAccountModal } from "~/components/ChangeAccountModal";
import { CloneModal } from "~/components/CloneModal";
import { CommitImageModal } from "~/components/CommitImageModal";
import { ImportAccountModal } from "~/components/ImportAccountModal";
import { SettingsPanel } from "~/components/SettingsPanel";
import { SetupWizard } from "~/components/SetupWizard";
import { Sidebar } from "~/components/Sidebar";
import {
  activate,
  applyMonitors,
  cloneHost,
  commitImage,
  deleteHost,
  deleteImage,
  getConfig,
  listImages,
  pullTemplate,
  putConfig,
  refreshClaudeUsage,
  reorder,
  swapClaudeAccount,
  testConfig,
} from "~/lib/api";
import { type ControlState, type Host, emptyState } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

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

/** Initial state from the SSR loader, kept live by the SSE stream. The same connection
 *  carries the persisted `ControlState` (default event) and a volatile per-host CPU/RAM
 *  map (named `stats` event) — the latter never touches `state.json`. */
function useLiveState(initial: ControlState) {
  const [state, setState] = useState(initial);
  const [stats, setStats] = useState<Record<string, ContainerStats>>({});
  useEffect(() => {
    const es = new EventSource("/events");
    es.onmessage = (e) => {
      try {
        setState(JSON.parse(e.data));
      } catch {
        // ignore malformed frame
      }
    };
    es.addEventListener("stats", (e) => {
      try {
        setStats(JSON.parse((e as MessageEvent).data));
      } catch {
        // ignore malformed frame
      }
    });
    return () => es.close();
  }, []);
  return { state, stats };
}

export default function Home({ loaderData }: Route.ComponentProps) {
  // The live SSE state powers both the wizard (template-provision progress) and the
  // dashboard, so it lives here at the gate. `stats` is the volatile per-host usage map.
  const { state, stats } = useLiveState(loaderData);
  // First-run gate: hold the config (null while loading). Render a minimal centered
  // "Loading…" until it resolves so the dashboard never flashes before the wizard
  // decision; render the wizard INSTEAD of the dashboard while setup isn't complete.
  const [cfg, setCfg] = useState<AppConfigRedacted | null>(null);
  const refetchConfig = () => {
    getConfig()
      .then(setCfg)
      .catch(() => setCfg(null));
  };
  useEffect(() => {
    refetchConfig();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!cfg) {
    return (
      <div className="flex h-screen items-center justify-center text-sm text-slate-400">
        Loading…
      </div>
    );
  }
  if (!cfg.setupComplete) {
    return <SetupWizard state={state} initialConfig={cfg} onDone={refetchConfig} />;
  }
  return <Dashboard state={state} stats={stats} cloneCpus={cfg.docker.cloneCpus} />;
}

function Dashboard({
  state,
  stats,
  cloneCpus,
}: {
  state: ControlState;
  stats: Record<string, ContainerStats>;
  cloneCpus: number;
}) {
  const [error, setError] = useState<string | null>(null);
  const [cloneOpen, setCloneOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [commitHost, setCommitHost] = useState<Host | null>(null);
  const [committing, setCommitting] = useState(false);
  const [changeHost, setChangeHost] = useState<Host | null>(null);
  const [changing, setChanging] = useState(false);

  // Clone-source images (from /api/images) — fetched on mount and refetched
  // whenever a pull/commit/delete op leaves `running` (the image set changed).
  const [images, setImages] = useState<ImageInfo[]>([]);
  const [imagesLoading, setImagesLoading] = useState(true);
  const refreshImages = () => {
    setImagesLoading(true);
    listImages()
      .then(setImages)
      .catch(() => {
        /* keep the last-known list on a transient error */
      })
      .finally(() => setImagesLoading(false));
  };
  useEffect(() => {
    refreshImages();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
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

  // Refetch images when an image-mutating op (pull/commit/delete) leaves the
  // running set — that's when the image list changed. Keyed on the set of running
  // op ids so it fires on each transition, not on every SSE frame.
  const imgOpsRunning = state.operations
    .filter(
      (o) =>
        o.status === "running" &&
        (o.kind === "pull" || o.kind === "commit" || o.kind === "delete"),
    )
    .map((o) => o.id)
    .join(",");
  useEffect(() => {
    refreshImages();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [imgOpsRunning]);

  const run = (p: Promise<unknown>) =>
    p.then(() => setError(null)).catch((e: Error) => setError(e.message));

  // Drag-reorder: optimistically adopt the new order (smooth UI), then persist —
  // the SSE frame confirms after the server writes it.
  const onReorder = (next: string[]) => {
    setOrder(next);
    run(reorder(next));
  };

  const runningClone = state.operations.some(
    (o) => o.kind === "clone" && o.status === "running",
  );

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
        <span className="min-w-0 flex-1 break-words text-sm font-semibold text-slate-800">
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

        {/* Left: host selection sidebar. Off-canvas drawer < lg, static ≥ lg.
            Presentational — every server call is a callback wired up here. */}
        <Sidebar
          open={sidebarOpen}
          accounts={state.claudeAccounts ?? []}
          hosts={orderedHosts}
          stats={stats}
          operations={state.operations}
          selectedId={state.selected}
          cloneCpus={cloneCpus}
          onOpenSettings={() => setSettingsOpen(true)}
          onOpenClone={() => setCloneOpen(true)}
          onRefreshClaude={() => run(refreshClaudeUsage())}
          onImportAccount={() => setImportOpen(true)}
          onSelectHost={(host) => {
            run(activate(host.id));
            setSidebarOpen(false);
          }}
          onDeleteHost={(host) => {
            const msg = host.managed
              ? `Delete ${host.id}? This destroys its container.`
              : `Remove ${host.id}? This unregisters the host.`;
            if (confirm(msg)) run(deleteHost(host.id));
          }}
          onCommitHost={(host) => setCommitHost(host)}
          onChangeAccountHost={(host) => setChangeHost(host)}
          onReorder={onReorder}
        />

        {/* Right: per-host editor */}
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden bg-white">
          {selectedHost ? (
            <>
              {/* Per-host header — only ≥ lg; on mobile the top bar shows id + tabs. */}
              <div className="hidden shrink-0 items-center gap-3 border-b border-slate-100 px-4 py-3 sm:px-6 lg:flex">
                <h2 className="min-w-0 break-words text-base font-semibold text-slate-900">
                  {selectedHost.id}
                </h2>
                <span className="shrink-0 text-xs text-slate-400">
                  {selectedHost.host}:{selectedHost.port}
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

      {cloneOpen ? (
        <CloneModal
          images={images}
          imagesLoading={imagesLoading}
          busy={runningClone}
          accounts={(state.claudeAccounts ?? []).filter(
            (a) => a.assignable && a.provider !== "codex",
          )}
          onClose={() => setCloneOpen(false)}
          onClone={(image, payload) => {
            run(cloneHost(image, payload));
            setCloneOpen(false);
          }}
        />
      ) : null}

      {settingsOpen ? (
        <SettingsPanel
          accountEmails={(state.claudeAccounts ?? [])
            .filter((a) => a.provider !== "codex")
            .map((a) => a.email)}
          onClose={() => setSettingsOpen(false)}
          getConfig={getConfig}
          putConfig={putConfig}
          testConfig={testConfig}
          applyMonitors={applyMonitors}
          images={images}
          imagesLoading={imagesLoading}
          pullBusy={state.operations.some(
            (o) => o.kind === "pull" && o.status === "running",
          )}
          onPullTemplate={(name, reference) => run(pullTemplate(name, reference))}
          onDeleteImage={(reference) => run(deleteImage(reference))}
        />
      ) : null}

      {commitHost ? (
        <CommitImageModal
          hostId={commitHost.id}
          busy={committing}
          onClose={() => setCommitHost(null)}
          onCommit={(name) => {
            setCommitting(true);
            commitImage(commitHost.id, name)
              .then(() => setError(null))
              .catch((e: Error) => setError(e.message))
              .finally(() => {
                setCommitting(false);
                setCommitHost(null);
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
