import { lazy, Suspense, useEffect, useState } from "react";

import { AppShellV2, type ShellPane, type SideFocus } from "~/components/AppShellV2";
import { ChangeAccountModal } from "~/components/ChangeAccountModal";
import { CloneModal } from "~/components/CloneModal";
import { CommitImageModal } from "~/components/CommitImageModal";
import { ImportAccountModal } from "~/components/ImportAccountModal";
import { PortForwardModal } from "~/components/PortForwardModal";
import { SettingsPanel } from "~/components/SettingsPanel";
import { SetupWizard } from "~/components/SetupWizard";
import {
  activate,
  activateLayout,
  archiveClone,
  duplicateClone,
  commitImage,
  deleteClaudeAccount,
  deleteCodexAccount,
  deleteClone,
  deleteImage,
  getConfig,
  getUpdateStatus,
  listImages,
  pullTemplate,
  putBoardColumns,
  putConfig,
  putForwards,
  refreshClaudeUsage,
  refreshCodexUsage,
  restartServer,
  swapClaudeAccount,
  swapCodexAccount,
  testConfig,
  unarchiveClone,
  updateServer,
} from "~/lib/api";
import {
  columnIdOf,
  moveCard,
  newColumnId,
  removeColumn,
  resolveColumns,
  withDefaults,
  type BoardColumn,
} from "~/lib/board";
import { type ControlState, type Clone, emptyState } from "~/lib/types";
import { useCloneNotifications } from "~/lib/useCloneNotifications";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";
import type { LxcStats } from "~/lib/wire/LxcStats";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

import type { Route } from "./+types/_index";

// BlockNote + the chat panel are browser-only; load them lazily and render only
// after mount so they never participate in SSR.
const CloneEditor = lazy(() => import("~/components/CloneEditor"));
const ChatPanel = lazy(() => import("~/components/ChatPanel"));

function ClientOnly({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  return mounted ? <>{children}</> : null;
}

export function meta() {
  return [{ title: "RMNG" }];
}

// SPA mode (ssr:false): the live EventSource("/events") delivers the initial full
// state on connect, so the loader just seeds an empty state client-side.
export function clientLoader() {
  return emptyState();
}

// If no frame or heartbeat has arrived for this long, treat the socket as wedged and
// rebuild it. The server pings every 15s, so ~3 missed pings — comfortably clear of
// jitter but quick enough to recover.
const SSE_STALE_MS = 45_000;

// How long to wait before reloading onto a new server build. Long enough for the notes
// editor's 600ms autosave debounce to fire, short enough that nobody reads a stale page.
const RELOAD_DELAY_MS = 1_500;

/** Initial state from the SSR loader, kept live by the SSE stream. The same connection
 *  carries persisted `ControlState` plus volatile clone (`stats`) and CT-wide (`lxcStats`)
 *  resource events; neither metric stream touches `state.json`.
 *
 *  A plain `EventSource` auto-reconnects when the browser *notices* the socket died — but
 *  after a Wi-Fi drop the TCP connection often goes half-open: it stays `OPEN`, delivers
 *  nothing, and never fires `onerror`, so the UI silently stops updating. We defend against
 *  that with an observable heartbeat + three recovery triggers: a staleness watchdog, the
 *  `online` event (Wi-Fi came back), and tab re-focus. Each rebuilds the connection, and a
 *  fresh `/events` connection replays the full snapshot, so the UI resyncs on reconnect. */
function useLiveState(initial: ControlState) {
  const [state, setState] = useState(initial);
  const [stats, setStats] = useState<Record<string, ContainerStats>>({});
  const [lxcStats, setLxcStats] = useState<LxcStats | null>(null);
  const [forwards, setForwards] = useState<Record<string, ForwardRuntime[]>>({});
  useEffect(() => {
    let es: EventSource | null = null;
    let lastActivity = Date.now();
    let disposed = false; // set on unmount so late callbacks don't reopen
    // The build this page belongs to, learned from the first `version` frame. The server
    // sends one per connection, and an upgrade drops every connection, so the reconnect
    // after an update is where a different id shows up.
    let buildId: string | null = null;

    const connect = () => {
      if (disposed) return;
      es?.close();
      lastActivity = Date.now();
      es = new EventSource("/events");
      es.onopen = () => {
        lastActivity = Date.now();
      };
      es.onmessage = (e) => {
        lastActivity = Date.now();
        try {
          setState(JSON.parse(e.data));
        } catch {
          // ignore malformed frame
        }
      };
      es.addEventListener("stats", (e) => {
        lastActivity = Date.now();
        try {
          setStats(JSON.parse((e as MessageEvent).data));
        } catch {
          // ignore malformed frame
        }
      });
      es.addEventListener("lxcStats", (e) => {
        lastActivity = Date.now();
        try {
          setLxcStats(JSON.parse((e as MessageEvent).data));
        } catch {
          // ignore malformed frame
        }
      });
      es.addEventListener("forwards", (e) => {
        lastActivity = Date.now();
        try {
          setForwards(JSON.parse((e as MessageEvent).data));
        } catch {
          // ignore malformed frame
        }
      });
      // Heartbeat carries no payload — it exists only to keep `lastActivity` fresh so the
      // watchdog can distinguish a wedged socket from an idle-but-healthy one.
      es.addEventListener("ping", () => {
        lastActivity = Date.now();
      });
      // The server restarted onto a different build, so this page's bundle is stale: its
      // JavaScript may be calling routes that moved or reading fields that changed shape.
      // Reload rather than let it keep talking to a server it wasn't built against.
      es.addEventListener("version", (e) => {
        lastActivity = Date.now();
        let next: string | null = null;
        try {
          next = (JSON.parse((e as MessageEvent).data) as { buildId?: string }).buildId ?? null;
        } catch {
          return; // malformed frame — leave the page alone
        }
        if (!next) return;
        if (buildId === null) {
          buildId = next;
          return;
        }
        if (next !== buildId) {
          disposed = true; // stop the watchdog rebuilding the socket under a reloading page
          es?.close();
          window.setTimeout(() => window.location.reload(), RELOAD_DELAY_MS);
        }
      });
    };

    connect();

    // Watchdog: rebuild a socket the browser has given up on (CLOSED), or one that still
    // claims to be OPEN but has gone silent past the staleness window (the half-open case).
    // A CONNECTING socket is the browser's own retry in flight — leave it be.
    const watchdog = window.setInterval(() => {
      if (disposed) return;
      const stale = Date.now() - lastActivity > SSE_STALE_MS;
      if (es?.readyState === EventSource.CLOSED || (es?.readyState === EventSource.OPEN && stale)) {
        connect();
      }
    }, 5_000);

    // Wi-Fi/network regained → rebuild immediately (the current socket is likely half-open).
    const onOnline = () => connect();
    // Tab re-focus after a sleep/background stretch that outran the staleness window.
    const onVisible = () => {
      if (document.visibilityState === "visible" && Date.now() - lastActivity > SSE_STALE_MS) {
        connect();
      }
    };
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      disposed = true;
      window.clearInterval(watchdog);
      window.removeEventListener("online", onOnline);
      document.removeEventListener("visibilitychange", onVisible);
      es?.close();
    };
  }, []);
  return { state, stats, lxcStats, forwards };
}

export default function Home({ loaderData }: Route.ComponentProps) {
  // The live SSE state powers both the wizard (template-provision progress) and the
  // dashboard, so it lives here at the gate. `stats` is the volatile per-clone usage map.
  const { state, stats, lxcStats, forwards } = useLiveState(loaderData);
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
      <div className="flex h-screen items-center justify-center text-sm text-slate-400 dark:text-slate-500">
        Loading…
      </div>
    );
  }
  if (!cfg.setupComplete) {
    return <SetupWizard state={state} initialConfig={cfg} onDone={refetchConfig} />;
  }
  return (
    <Dashboard
      state={state}
      stats={stats}
      lxcStats={lxcStats}
      forwards={forwards}
      sshPublicHost={cfg.ssh?.publicHost ?? ""}
      bastionPort={cfg.listen.bastion}
    />
  );
}
function Dashboard({
  state,
  stats,
  lxcStats,
  forwards,
  sshPublicHost,
  bastionPort,
}: {
  state: ControlState;
  stats: Record<string, ContainerStats>;
  lxcStats: LxcStats | null;
  forwards: Record<string, ForwardRuntime[]>;
  /** `ssh.publicHost` (config) — threaded down to each card's copied SSH command;
   *  empty ⇒ falls back to `window.location.hostname`. */
  sshPublicHost: string;
  /** `listen.bastion` — the bastion `sshd` port the copied SSH commands jump through. */
  bastionPort: number;
}) {
  // OS notification whenever a clone transitions out of `working` (idle/offline) while
  // it isn't the selected one — driven by the server's `unread` edge. Clicking it selects
  // that clone, the same activate path a card click uses.
  useCloneNotifications(state.hosts, (id) => run(activate(id)));

  const [error, setError] = useState<string | null>(null);
  const [cloneOpen, setCloneOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [commitClone, setCommitClone] = useState<Clone | null>(null);
  const [committing, setCommitting] = useState(false);
  const [changeClone, setChangeClone] = useState<Clone | null>(null);
  const [changing, setChanging] = useState(false);
  // The group an "add account" OAuth login is in flight for (null = modal closed).
  const [importOpen, setImportOpen] = useState(false);
  const [forwardClone, setForwardClone] = useState<Clone | null>(null);
  const [forwarding, setForwarding] = useState(false);
  const [forwardError, setForwardError] = useState<string | null>(null);

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
  // Below `lg` the board and the side panel cannot share the screen, so one of the three
  // panes wins. Above it, `sideFocus` decides which half of the side panel gets the height.
  const [pane, setPane] = useState<ShellPane>("board");
  const [sideFocus, setSideFocus] = useState<SideFocus>("notes");

  // Board columns, held locally so a drag lands instantly, and re-adopted whenever the
  // server's own list changes. Keyed on the serialized list rather than the array identity:
  // every SSE frame brings a fresh array, and resetting on each one would undo a drag that
  // is still in flight.
  const serverColumns = JSON.stringify(state.boardColumns ?? []);
  const [columns, setColumns] = useState<BoardColumn[]>(() =>
    withDefaults(state.boardColumns ?? []),
  );
  useEffect(() => {
    setColumns(withDefaults(JSON.parse(serverColumns) as BoardColumn[]));
  }, [serverColumns]);

  const clonesById = new Map(state.hosts.map((h) => [h.id, h]));
  const selectedClone = state.selected ? clonesById.get(state.selected) ?? null : null;

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

  // Adopt a new column layout locally, then persist it. The board previews a drag on its
  // own, so by the time this runs the operator has already seen the result; the PUT is
  // what makes it survive a refresh.
  const applyColumns = (next: BoardColumn[]) => {
    setColumns(next);
    run(putBoardColumns(next));
  };

  // A clone being provisioned into a specific column. The board can only file a clone that
  // exists, and the clone does not exist until its op finishes, so the request is parked
  // here and applied when the clone shows up in `hosts`.
  const [pendingColumn, setPendingColumn] = useState<{ columnId: string; target: string } | null>(
    null,
  );
  const [newCloneColumn, setNewCloneColumn] = useState<string | null>(null);
  useEffect(() => {
    if (!pendingColumn) return;
    const { columnId, target } = pendingColumn;
    if (!state.hosts.some((h) => h.id === target)) return;
    setPendingColumn(null);
    if (columnIdOf(columns, target) === columnId) return;
    applyColumns(moveCard(columns, target, columnId, 0));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.hosts, pendingColumn]);

  // Imported accounts, split per provider. `claudeAccounts` carries BOTH providers' rows
  // tagged by `provider`; a row written before that field existed has none, which means
  // Claude — so the Claude side matches on "not codex" rather than on "is claude".
  const accounts = state.claudeAccounts ?? [];
  const claudeAccounts = accounts.filter((a) => a.provider !== "codex");
  const codexAccounts = accounts.filter((a) => a.provider === "codex");

  const onDeleteAccount = (email: string) =>
    run(deleteClaudeAccount(email).then(() => refreshClaudeUsage()));
  const onDeleteCodexAccount = (email: string) =>
    run(deleteCodexAccount(email).then(() => refreshCodexUsage()));

  // Archiving rides a drag into the Archived column, and that column's contents come from
  // the server's `archived` flag rather than from the column list. So when the call fails
  // the card has to go back where it was: `before` is the layout from before the drop, the
  // board having fired this callback ahead of the move.
  const archiveDrop = (clone: Clone, archived: boolean) => {
    const before = columns;
    const call = archived ? archiveClone(clone.id) : unarchiveClone(clone.id);
    call
      .then(() => setError(null))
      .catch((e: Error) => {
        setError(e.message);
        applyColumns(before);
      });
  };

  // The selected clone, when it can actually parent a sub clone: managed, and top-level
  // (sub clones are one level deep — the server rejects a sub clone as a parent).
  const subCloneParent =
    selectedClone?.managed && !selectedClone.parent ? selectedClone : null;

  // Clones per column, for the settings editor's counts (unfiled ones ride the first).
  const columnCounts = Object.fromEntries(
    resolveColumns(columns, state.hosts).map((c) => [c.id, c.cloneIds.length]),
  );

  return (
    <AppShellV2
      selectedClone={selectedClone}
      error={error}
      pane={pane}
      onPaneChange={setPane}
      sideFocus={sideFocus}
      onSideFocusChange={setSideFocus}
      rail={{
        accounts,
        lxcStats,
        operations: state.operations,
        presetNames: state.layoutPresetNames ?? [],
        activeLayout: state.activeLayout ?? "",
        onActivateLayout: (name) => run(activateLayout(name)),
        onOpenSettings: () => setSettingsOpen(true),
        onImportAccount: () => setImportOpen(true),
        onRefresh: () => {
          void Promise.all([refreshClaudeUsage(), refreshCodexUsage()]);
        },
      }}
      board={{
        columns,
        clones: state.hosts,
        stats,
        cloneTokens: state.cloneTokens,
        forwards,
        operations: state.operations,
        selectedId: state.selected,
        sshPublicHost,
        bastionPort,
        onSelectClone: (clone) => run(activate(clone.id)),
        onDeleteClone: (clone) => {
          // Deleting a parent cascades to its sub clones (server-side), so say so up front.
          const subCount = state.hosts.filter((h) => h.parent === clone.id).length;
          const subs =
            subCount > 0 ? ` and its ${subCount} sub clone${subCount === 1 ? "" : "s"}` : "";
          const msg = clone.managed
            ? `Delete ${clone.id}${subs}? This destroys its container${subCount > 0 ? "s" : ""}.`
            : `Remove ${clone.id}? This unregisters the clone.`;
          if (confirm(msg)) run(deleteClone(clone.id));
        },
        onCommitClone: (clone) => setCommitClone(clone),
        onChangeAccountClone: (clone) => setChangeClone(clone),
        onPortForwardClone: (clone) => {
          setForwardError(null);
          setForwardClone(clone);
        },
        onArchiveClone: (clone) => archiveDrop(clone, true),
        onUnarchiveClone: (clone) => archiveDrop(clone, false),
        onNewClone: (columnId) => {
          setNewCloneColumn(columnId);
          setCloneOpen(true);
        },
        onMoveCard: (cloneId, toColumnId, toIndex) =>
          applyColumns(moveCard(columns, cloneId, toColumnId, toIndex)),
        onRenameColumn: (columnId, title) =>
          applyColumns(columns.map((c) => (c.id === columnId ? { ...c, title } : c))),
      }}
      notes={
        selectedClone ? (
          <ClientOnly>
            <Suspense
              fallback={
                <div className="p-6 text-sm text-slate-400 dark:text-slate-500">Loading editor…</div>
              }
            >
              <CloneEditor key={selectedClone.id} cloneId={selectedClone.id} />
            </Suspense>
          </ClientOnly>
        ) : null
      }
      chat={
        selectedClone ? (
          <ClientOnly>
            <Suspense
              fallback={
                <div className="p-4 text-sm text-slate-400 dark:text-slate-500">Loading chat…</div>
              }
            >
              <ChatPanel
                key={selectedClone.id}
                cloneId={selectedClone.id}
                archived={selectedClone.archived === true}
              />
            </Suspense>
          </ClientOnly>
        ) : null
      }
      overlays={
        <>
          {cloneOpen ? (
            <CloneModal
              images={images}
              imagesLoading={imagesLoading}
              operations={state.operations}
              parentCandidate={subCloneParent}
              accounts={accounts}
              onClose={() => {
                setCloneOpen(false);
                setNewCloneColumn(null);
              }}
              // The dialog owns the whole lifecycle now: it keeps itself open, renders the op's
              // progress, and closes when the op settles. So this just starts it and hands the
              // Operation back — errors surface inside the dialog, not in the page banner.
              // The op's target is the new clone's id, which is how it reaches the column
              // whose button opened this.
              onClone={(image, payload) =>
                duplicateClone(image, payload).then((op) => {
                  if (newCloneColumn) {
                    setPendingColumn({ columnId: newCloneColumn, target: op.target });
                  }
                  return op;
                })
              }
            />
          ) : null}

          {settingsOpen ? (
            <SettingsPanel
              accounts={accounts}
              onClose={() => setSettingsOpen(false)}
              getConfig={getConfig}
              putConfig={putConfig}
              testConfig={testConfig}
              getUpdateStatus={getUpdateStatus}
              updateServer={updateServer}
              operations={state.operations}
              restartServer={restartServer}
              images={images}
              imagesLoading={imagesLoading}
              pullBusy={state.operations.some(
                (o) => o.kind === "pull" && o.status === "running",
              )}
              onPullTemplate={(reference) => run(pullTemplate(reference))}
              onDeleteImage={(reference) => run(deleteImage(reference))}
              onImportAccount={() => setImportOpen(true)}
              onDeleteAccount={onDeleteAccount}
              onDeleteCodexAccount={onDeleteCodexAccount}
              boardColumns={columns}
              boardColumnCounts={columnCounts}
              onAddBoardColumn={(title) =>
                applyColumns([
                  ...columns,
                  { id: newColumnId(title, columns), title, cloneIds: [] },
                ])
              }
              onRenameBoardColumn={(columnId, title) =>
                applyColumns(columns.map((c) => (c.id === columnId ? { ...c, title } : c)))
              }
              onDeleteBoardColumn={(columnId) => applyColumns(removeColumn(columns, columnId))}
              onReorderBoardColumns={(ids) =>
                applyColumns(
                  ids.flatMap((id) => {
                    const column = columns.find((c) => c.id === id);
                    return column ? [column] : [];
                  }),
                )
              }
            />
          ) : null}

          {commitClone ? (
            <CommitImageModal
              cloneId={commitClone.id}
              busy={committing}
              onClose={() => setCommitClone(null)}
              onCommit={(name) => {
                setCommitting(true);
                commitImage(commitClone.id, name)
                  .then(() => setError(null))
                  .catch((e: Error) => setError(e.message))
                  .finally(() => {
                    setCommitting(false);
                    setCommitClone(null);
                  });
              }}
            />
          ) : null}

          {importOpen ? (
            <ImportAccountModal
              clones={state.hosts}
              onClose={() => setImportOpen(false)}
              onImported={() => {
                setImportOpen(false);
                // The account is in the server's store immediately; its usage windows fill in on
                // the next poll, which these kick off rather than waiting out the interval.
                setError(null);
                run(refreshClaudeUsage());
                run(refreshCodexUsage());
              }}
            />
          ) : null}

          {changeClone ? (
            <ChangeAccountModal
              clone={changeClone}
              accounts={claudeAccounts}
              codexAccounts={codexAccounts}
              busy={changing}
              onClose={() => setChangeClone(null)}
              onSubmit={(claude, codex) => {
                setChanging(true);
                Promise.all([
                  swapClaudeAccount(changeClone.id, claude),
                  swapCodexAccount(changeClone.id, codex),
                ])
                  .then(() => {
                    setError(null);
                    setChangeClone(null);
                  })
                  .catch((e: Error) => setError(e.message))
                  .finally(() => setChanging(false));
              }}
            />
          ) : null}

          {forwardClone ? (
            <PortForwardModal
              clone={state.hosts.find((h) => h.id === forwardClone.id) ?? forwardClone}
              runtime={forwards[forwardClone.id] ?? []}
              busy={forwarding}
              error={forwardError}
              onClose={() => setForwardClone(null)}
              onSubmit={(list) => {
                setForwarding(true);
                setForwardError(null);
                putForwards(forwardClone.id, list)
                  .then(() => setForwardClone(null))
                  .catch((e: Error) => setForwardError(e.message))
                  .finally(() => setForwarding(false));
              }}
            />
          ) : null}
        </>
      }
    />
  );
}
