// The /ng tab pages' container: it holds what the five pages draw (the live server state, the
// Linear tickets), does what they ask (every write the dashboard can make), and opens the
// dialogs. It is the phone app's counterpart of `DashboardContainer`, with the same writes in
// the same shape: show the change now, send it, put the old value back if it is refused.
//
// Narrow, the pages are tabs under the tool row (`../ToolRow.tsx`). Each page, once opened,
// stays mounted while another one shows, so a half-typed setting, a scrolled list or an open
// ticket is still there when the operator comes back. A dialog opens in the page that opened
// it, in that page's space (`ModalPlacementProvider`), so the clone's screen stays in view.
//
// Wide (`WIDE_MIN`), they are the dashboard's layout (`./WideLayout.tsx`): the usage rail, the
// board's columns, the side panel; a dialog opens over them, in the middle.
import { lazy, type ReactNode, Suspense, useEffect, useRef, useState } from "react";

import { ChangeAccountModalContainer } from "~/components/ChangeAccountModalContainer";
import { CloneModalContainer } from "~/components/CloneModalContainer";
import { ImportAccountModalContainer } from "~/components/ImportAccountModalContainer";
import { ModalPlacementProvider } from "~/components/ModalShell";
import { PortForwardModal } from "~/components/PortForwardModal";
import { RebaseModalContainer } from "~/components/RebaseModalContainer";
import { useSettingsPanel } from "~/components/SettingsPanelContainer";
import { TicketModalContainer } from "~/components/TicketModalContainer";
import { TicketPanel } from "~/components/TicketPanel";
import { useAccountOrder } from "~/lib/accountOrder";
import {
  activate,
  archiveClone,
  deleteClone,
  getConfig,
  getUpdateStatus,
  putBoardColumns,
  putConfig,
  putMutedClones,
  putTicketOrder,
  rebaseClone,
  refreshClaudeUsage,
  refreshCodexUsage,
  restartServer,
  startClone,
  swapClaudeAccount,
  swapCodexAccount,
  testConfig,
  unarchiveClone,
  updateServer,
} from "~/lib/api";
import {
  archivesOnDrop,
  type BoardColumn,
  columnIdOf,
  moveCard,
  newColumnId,
  removeColumn,
  resolveColumns,
  withDefaults,
} from "~/lib/board";
import { copyText } from "~/lib/clipboard";
import { issueSetLabel, issueSetState, issueSetStateId, issueUpdate, keysForTeam } from "~/lib/linear/mutations";
import type { TicketLabel, TicketWorkflowState } from "~/lib/linear/types";
import { useTeamMeta } from "~/lib/linear/useTeamMeta";
import { useTickets } from "~/lib/linear/useTickets";
import { useWorkspaces } from "~/lib/linear/useWorkspaces";
import { workspaceHomeUrl } from "~/lib/linear/workspaces";
import { toggleMuted } from "~/lib/mute";
import { BASTION_PORT, buildSshCommand } from "~/lib/ssh";
import {
  adoptTickets,
  cloneForTicket,
  cloneTickets,
  findTicket,
  type LinearTicket,
  openTickets,
  orderTickets,
  queued,
  ticketAfter,
  type TicketState,
} from "~/lib/tickets";
import type { ClaudeUsage, Clone, ControlState } from "~/lib/types";
import { useNow } from "~/lib/useNow";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import type { SoundReport } from "~/viewer/protocol";

import { type NgTab, ROW_STATS_MIN, type ServerUsage, TABS as TAB_INFO } from "../ToolRow";
import { ClonesView } from "./ClonesView";
import { NotesView, type NotesPane } from "./NotesView";
import { type Move, moved } from "./order";
import { SettingsView } from "./SettingsView";
import { TicketsView } from "./TicketsView";
import { ErrorLine, PAGES_GLASS } from "./ui";
import { BoardDnd } from "./BoardDnd";
import { NarrowLayout } from "./NarrowLayout";
import { UsageView } from "./UsageView";
import { WIDE_MIN, WideLayout } from "./WideLayout";

// Browser-only and heavy (BlockNote, the chat stream): loaded when first shown
const NotesEditorContainer = lazy(() => import("~/components/NotesEditorContainer"));
const ChatContainer = lazy(() => import("~/components/ChatContainer"));
const TicketDescription = lazy(() => import("~/components/TicketDescription"));

const TABS: NgTab[] = ["usage", "tickets", "clones", "notes", "settings"];

function Loading({ label }: { label: string }) {
  return <p className="p-4 text-sm text-black/40 dark:text-white/40">{label}</p>;
}

/** A dialog that is open, and the page it opened in. */
type Dialog =
  | { kind: "clone"; ticketUrl: string; columnId: string | null }
  | { kind: "ticket" }
  | { kind: "import"; replacing: ClaudeUsage | null; provider?: "claude" | "codex"; group?: string }
  | { kind: "account"; clone: Clone }
  | { kind: "rebase"; clone: Clone }
  | { kind: "forward"; clone: Clone };

export function NgPagesContainer({
  width,
  glass,
  tab,
  onTab,
  usage,
  badges,
  onPopOut,
  state,
  stats,
  sound,
  sideAt,
  onLayout,
}: {
  /** The pages' width, CSS px: narrow (tabs) or wide (the dashboard's layout). */
  width: number;
  /** Over the clone's screen: wide, each column is glass of its own (narrow, the screen's view
   *  puts the whole pages on one glass panel). Not in the pages' own window. */
  glass: boolean;
  /** The server's CPU, memory and disk; null until they arrive. */
  usage: ServerUsage | null;
  /** A count on a tab. */
  badges?: Partial<Record<NgTab, number>>;
  /** Move the pages to a window of their own; absent where the browser opens none. */
  onPopOut?: () => void;
  /** The page that shows; null while the keyboard shows. */
  tab: NgTab | null;
  /** Show another page, or the keyboard (null). */
  onTab: (tab: NgTab | null) => void;
  state: ControlState;
  stats: Record<string, ContainerStats>;
  /** How the clone's sound plays on this phone; null before it plays. */
  sound: SoundReport | null;
  /** Wide, over the clone's screen: the screen's box for the side panels, which shows them
   *  without the pages too (`./WideLayout.tsx`); null until it is there. */
  sideAt?: HTMLElement | null;
  /** Open the Edit layout dialog; absent where there is none (the pages' own window). */
  onLayout?: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const run = (p: Promise<unknown>) => p.then(() => setError(null)).catch((e: Error) => setError(e.message));
  const { acctOrder } = useAccountOrder();
  const now = useNow();
  const sshHost = typeof window === "undefined" ? "" : window.location.hostname;

  // The presets carry the Linear keys this browser reads tickets with
  const [presets, setPresets] = useState<PresetRedacted[]>([]);
  useEffect(() => {
    getConfig()
      .then((c) => setPresets(c.presets))
      .catch((e: Error) => setError(e.message));
  }, []);

  // The pages opened so far stay mounted (see the top of this file)
  const [visited, setVisited] = useState<ReadonlySet<NgTab>>(new Set());
  useEffect(() => {
    if (tab && !visited.has(tab)) setVisited(new Set([...visited, tab]));
  }, [tab, visited]);

  const [dialog, setDialog] = useState<{ on: NgTab; d: Dialog } | null>(null);
  /** The change-account dialog's call is in flight. */
  const [changing, setChanging] = useState(false);
  const open = (d: Dialog) => tab && setDialog({ on: tab, d });
  const closeDialog = () => setDialog(null);

  const clonesById = new Map(state.hosts.map((h) => [h.id, h]));
  const accounts = state.claudeAccounts ?? [];

  // ---- the board's columns, the ticket order and the mutes: shown now, then stored ----
  const serverColumns = JSON.stringify(state.boardColumns ?? []);
  const [columns, setColumns] = useState<BoardColumn[]>(() => withDefaults(state.boardColumns ?? []));
  useEffect(() => setColumns(withDefaults(JSON.parse(serverColumns) as BoardColumn[])), [serverColumns]);
  const applyColumns = (next: BoardColumn[]) => {
    setColumns(next);
    run(putBoardColumns(next));
  };

  const serverOrder = JSON.stringify(state.ticketOrder ?? []);
  const [ticketOrder, setTicketOrder] = useState<string[]>(() => state.ticketOrder ?? []);
  useEffect(() => setTicketOrder(JSON.parse(serverOrder) as string[]), [serverOrder]);
  const applyTicketOrder = (next: string[]) => {
    setTicketOrder(next);
    run(putTicketOrder(next));
  };

  const serverMuted = JSON.stringify(state.mutedClones ?? []);
  const [mutedClones, setMutedClones] = useState<string[]>(() => state.mutedClones ?? []);
  useEffect(() => setMutedClones(JSON.parse(serverMuted) as string[]), [serverMuted]);

  // ---- Linear ----
  const {
    tickets: linearTickets,
    error: ticketsError,
    loading: ticketsLoading,
    refetch: refetchTickets,
    upsert: upsertTicket,
  } = useTickets(
    presets,
    state.hosts.flatMap((h) => (h.linearTicket ? [h.linearTicket] : [])),
  );
  const workspaces = useWorkspaces(presets);
  const visibleTickets = orderTickets(openTickets(linearTickets, state.hosts), ticketOrder);
  // Each ticket keeps the place it was first drawn in (as on the desktop)
  useEffect(() => {
    const adopted = adoptTickets(visibleTickets, ticketOrder);
    if (adopted) applyTicketOrder(adopted);
  });
  const liveCloneTickets = cloneTickets(state.hosts, linearTickets);

  const [openTicketId, setOpenTicketId] = useState<string | null>(null);
  const openTicket = openTicketId ? findTicket(openTicketId, visibleTickets) : null;

  /** A Linear write: on screen now, in Linear a round trip later, put back if refused. */
  const writeTicket = (ticket: LinearTicket, next: LinearTicket, call: (keys: string[]) => Promise<unknown>) => {
    upsertTicket(next);
    run(
      call(keysForTeam(presets, ticket.team ?? ""))
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };
  /** The open ticket leaves the list: the page goes to the one after it. */
  const advancePast = (ticket: LinearTicket) => {
    if (openTicket?.id !== ticket.id) return;
    setOpenTicketId(ticketAfter(visibleTickets, ticket.id)?.id ?? null);
  };
  const editTicket = (t: LinearTicket, patch: { title?: string; description?: string }) =>
    writeTicket(t, { ...t, ...patch }, (keys) => issueUpdate(keys, t, patch));
  const moveTicket = (t: LinearTicket, next: TicketState) => {
    if (!queued(next)) advancePast(t);
    writeTicket(t, { ...t, state: next }, (keys) => issueSetState(keys, t, next));
  };
  const setTicketState = (t: LinearTicket, s: TicketWorkflowState) => {
    if (!queued(s.type)) advancePast(t);
    writeTicket(t, { ...t, state: s.type, stateId: s.id, stateName: s.name }, (keys) => issueSetStateId(keys, t, s.id));
  };
  const setTicketLabel = (t: LinearTicket, label: TicketLabel, on: boolean) =>
    writeTicket(
      t,
      { ...t, labels: on ? [...t.labels, label] : t.labels.filter((l) => l.id !== label.id) },
      (keys) => issueSetLabel(keys, t, label.id, on),
    );

  // ---- the clone on the screen, and the one the Notes page is on ----
  /** An archived clone the Notes page shows; null: the one on the screen. */
  const [notesId, setNotesId] = useState<string | null>(null);
  const notesClone = (notesId ? clonesById.get(notesId) : null) ?? (state.selected ? clonesById.get(state.selected) : null) ?? null;
  const [notesPane, setNotesPane] = useState<NotesPane>("chat");
  const notesTicket = notesClone?.linearTicket ? findTicket(notesClone.linearTicket, linearTickets) : null;
  const openTicketMeta = useTeamMeta(presets, openTicket);
  const notesTicketMeta = useTeamMeta(presets, notesTicket);

  /** Put a clone on the screen; the pages stay open (the operator closes them). An archived one
   *  has no screen, so its notes open instead. */
  const selectClone = (clone: Clone) => {
    if (clone.archived) {
      setNotesId(clone.id);
      onTab("notes");
      return;
    }
    setNotesId(null);
    if (state.selected !== clone.id) run(activate(clone.id));
  };

  /** Where a parent or sub-issue goes: the ticket in this page, or the clone made for it. */
  const resolveLink = (id: string, inList: boolean) => {
    const t = inList ? findTicket(id, visibleTickets) : null;
    if (t) return { title: `Show ${t.id}`, open: () => setOpenTicketId(t.id) };
    const clone = cloneForTicket(id, state.hosts);
    return clone ? { title: `Show the clone for ${id}: ${clone.id}`, open: () => selectClone(clone) } : null;
  };

  const ticketPanel = (t: LinearTicket, meta: ReturnType<typeof useTeamMeta>, own: boolean) => (
    <TicketPanel
      ticket={t}
      onCopyBranchName={copyText}
      description={
        <Suspense fallback={<Loading label="Loading…" />}>
          <TicketDescription key={t.id} markdown={t.description ?? ""} onSave={(md) => editTicket(t, { description: md })} />
        </Suspense>
      }
      onTitleChange={(title) => editTicket(t, { title })}
      onStateChange={(s) => setTicketState(t, s)}
      stateOptions={meta.states}
      statesLoading={meta.loading}
      labelOptions={meta.labels}
      labelsLoading={meta.loading}
      onAddLabel={(l) => setTicketLabel(t, l, true)}
      onRemoveLabel={(l) => setTicketLabel(t, l, false)}
      onCreateClone={own ? undefined : () => open({ kind: "clone", ticketUrl: t.url, columnId: null })}
      resolveLink={(id) => resolveLink(id, !own)}
    />
  );

  const notesSlot = () => {
    if (!notesClone) return null;
    if (notesClone.linearTicket && notesTicket) return ticketPanel(notesTicket, notesTicketMeta, true);
    const editor = (
      <Suspense fallback={<Loading label="Loading notes…" />}>
        <NotesEditorContainer key={notesClone.id} cloneId={notesClone.id} />
      </Suspense>
    );
    if (!notesClone.linearTicket) return editor;
    if (ticketsLoading) return <Loading label={`Loading ${notesClone.linearTicket}…`} />;
    return (
      <>
        <p className="px-4 pb-2 text-xs text-neutral-500">
          {notesClone.linearTicket} did not load{ticketsError ? `: ${ticketsError}` : ""}. Showing notes.
        </p>
        {editor}
      </>
    );
  };

  // ---- clones ----
  /** Into a column; into an archive column archives it and out of one restores it, as the
   *  board's drop does. A refused call puts the card back. */
  const moveToColumn = (clone: Clone, columnId: string, at?: number) => {
    const before = columns;
    const resolved = resolveColumns(columns, state.hosts);
    const index = at ?? resolved.find((c) => c.id === columnId)?.cloneIds.length ?? 0;
    applyColumns(moveCard(resolved, clone.id, columnId, index));
    const archive = archivesOnDrop(columns, columnId);
    if (archive === (clone.archived === true)) return;
    (archive ? archiveClone(clone.id) : unarchiveClone(clone.id))
      .then(() => setError(null))
      .catch((e: Error) => {
        setError(e.message);
        applyColumns(before);
      });
  };
  const moveInColumn = (clone: Clone, columnId: string, move: Move) => {
    const resolved = resolveColumns(columns, state.hosts);
    const ids = resolved.find((c) => c.id === columnId)?.cloneIds ?? [];
    const next = moved(ids, clone.id, move);
    applyColumns(moveCard(resolved, clone.id, columnId, next.indexOf(clone.id)));
  };
  const deleteOne = (clone: Clone) => {
    const subs = state.hosts.filter((h) => h.parent === clone.id).length;
    const also = subs > 0 ? ` and its ${subs} sub clone${subs === 1 ? "" : "s"}` : "";
    const msg = clone.managed
      ? `Delete ${clone.id}${also}? This destroys its container${subs > 0 ? "s" : ""}.`
      : `Remove ${clone.id}? This unregisters the clone.`;
    if (confirm(msg)) run(deleteClone(clone.id));
  };

  // A clone made into a column is filed there once it exists; the one just made goes on the
  // screen when its dialog closes
  const [pendingColumn, setPendingColumn] = useState<{ columnId: string; target: string } | null>(null);
  const newClone = useRef<string | null>(null);
  useEffect(() => {
    if (!pendingColumn) return;
    const { columnId, target } = pendingColumn;
    if (!state.hosts.some((h) => h.id === target)) return;
    setPendingColumn(null);
    if (columnIdOf(columns, target) !== columnId) applyColumns(moveCard(columns, target, columnId, 0));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.hosts, pendingColumn]);

  // ---- settings ----
  const columnCounts = Object.fromEntries(resolveColumns(columns, state.hosts).map((c) => [c.id, c.cloneIds.length]));
  const settings = useSettingsPanel({
    accounts,
    getConfig,
    putConfig,
    testConfig,
    getUpdateStatus,
    updateServer,
    operations: state.operations,
    restartServer,
    onImportAccount: (provider, group) => open({ kind: "import", replacing: null, provider, group }),
    forkSources: state.hosts.filter((h) => h.managed).map((h) => h.id),
    boardColumns: columns,
    boardColumnCounts: columnCounts,
    onAddBoardColumn: (title) =>
      applyColumns([...columns, { id: newColumnId(title, columns), title, cloneIds: [], archive: false }]),
    onRenameBoardColumn: (id, title) => applyColumns(columns.map((c) => (c.id === id ? { ...c, title } : c))),
    onSetBoardColumnArchive: (id, archive) => applyColumns(columns.map((c) => (c.id === id ? { ...c, archive } : c))),
    onDeleteBoardColumn: (id) => applyColumns(removeColumn(columns, id)),
    onReorderBoardColumns: (ids) =>
      applyColumns(ids.flatMap((id) => columns.filter((c) => c.id === id))),
  });

  const refreshUsage = () => Promise.all([refreshClaudeUsage(), refreshCodexUsage()]).then(() => undefined);

  const dialogView = (d: Dialog): ReactNode => {
    switch (d.kind) {
      case "clone":
        return (
          <CloneModalContainer
            clones={state.hosts}
            operations={state.operations}
            accounts={accounts}
            initialTicket={d.ticketUrl}
            onClose={() => {
              closeDialog();
              const made = newClone.current ? clonesById.get(newClone.current) : null;
              newClone.current = null;
              if (made) selectClone(made);
            }}
            onStart={(fork, req) =>
              startClone(fork, req).then((op) => {
                if (d.columnId) setPendingColumn({ columnId: d.columnId, target: op.target });
                newClone.current = op.target;
                return op;
              })
            }
          />
        );
      case "ticket":
        return (
          <TicketModalContainer
            presets={presets}
            onClose={closeDialog}
            onCreated={(created) => {
              upsertTicket(created);
              setOpenTicketId(created.id);
              refetchTickets();
            }}
          />
        );
      case "import":
        return (
          <ImportAccountModalContainer
            groupNames={state.groups.map((g) => g.name)}
            initialProvider={d.provider}
            initialGroup={d.group}
            replacing={
              d.replacing
                ? { provider: d.replacing.provider === "codex" ? "codex" : "claude", email: d.replacing.email }
                : null
            }
            onClose={closeDialog}
            onImported={() => {
              closeDialog();
              run(refreshUsage());
            }}
          />
        );
      case "account":
        return (
          <ChangeAccountModalContainer
            clone={d.clone}
            accounts={accounts.filter((a) => a.provider !== "codex")}
            codexAccounts={accounts.filter((a) => a.provider === "codex")}
            busy={changing}
            onClose={closeDialog}
            onSubmit={(claude, codex, group) => {
              setChanging(true);
              Promise.all([swapClaudeAccount(d.clone.id, claude, group), swapCodexAccount(d.clone.id, codex, group)])
                .then(() => {
                  setError(null);
                  closeDialog();
                })
                .catch((e: Error) => setError(e.message))
                .finally(() => setChanging(false));
            }}
          />
        );
      case "rebase":
        return (
          <RebaseModalContainer
            cloneId={d.clone.id}
            currentPreset={d.clone.presetName ?? null}
            operations={state.operations}
            onClose={closeDialog}
            onRebase={(preset, rebuild) => rebaseClone(d.clone.id, preset, rebuild)}
          />
        );
      case "forward":
        return <PortForwardModal clone={d.clone} sshPublicHost={sshHost} onClose={closeDialog} onCopy={copyText} />;
    }
  };
  // ---- drag and drop (`./BoardDnd.tsx`) ----
  const lanes = resolveColumns(columns, state.hosts).map((c) => ({ id: c.id, cloneIds: c.cloneIds }));
  const dropClone = (cloneId: string, columnId: string, index: number) => {
    const clone = clonesById.get(cloneId);
    if (clone) moveToColumn(clone, columnId, index);
  };
  const dropTicketOnColumn = (ticketId: string, columnId: string) => {
    const t = findTicket(ticketId, visibleTickets);
    if (t) open({ kind: "clone", ticketUrl: t.url, columnId });
  };
  /** A page with what drags in it: the clones between their columns, the tickets in order. */
  const withDnd = (t: NgTab, node: ReactNode): ReactNode =>
    t === "clones" ? (
      <BoardDnd lanes={lanes} onMoveCard={dropClone}>
        {node}
      </BoardDnd>
    ) : t === "tickets" ? (
      <BoardDnd tickets={visibleTickets.map((x) => x.id)} onReorderTickets={applyTicketOrder}>
        {node}
      </BoardDnd>
    ) : (
      node
    );

  /** The Notes page, or (`only`, wide) one of its two parts in a side panel of its own. */
  const notesView = (only?: NotesPane) => (
    <NotesView
      only={only}
      clone={notesClone}
      ticket={notesClone ? liveCloneTickets[notesClone.id] : undefined}
      pane={notesPane}
      onPaneChange={setNotesPane}
      notesLabel={notesClone?.linearTicket ? "Ticket" : "Notes"}
      notes={notesSlot()}
      chat={
        notesClone ? (
          <Suspense fallback={<Loading label="Loading chat…" />}>
            <ChatContainer key={notesClone.id} cloneId={notesClone.id} archived={notesClone.archived === true} />
          </Suspense>
        ) : null
      }
      onBackToScreen={notesId && notesId !== state.selected ? () => setNotesId(null) : undefined}
    />
  );

  /** A page, as the tabs show it, or (`wide`) as its part of the dashboard's layout. */
  const page = (t: NgTab, wide = false): ReactNode => {
    switch (t) {
      case "usage":
        return (
          <UsageView
            rail={wide}
            server={wide || width - 8 >= ROW_STATS_MIN ? undefined : usage}
            accounts={accounts}
            accountOrder={acctOrder}
            groups={state.groups}
            now={now}
            operations={state.operations}
            sound={sound}
            onRefresh={() => refreshUsage().catch((e: Error) => setError(e.message))}
            onReplaceAccount={(a) => open({ kind: "import", replacing: a })}
          />
        );
      case "tickets":
        return (
          <TicketsView
            column={wide}
            tickets={visibleTickets}
            loading={ticketsLoading}
            error={ticketsError}
            open={openTicket}
            detail={openTicket ? ticketPanel(openTicket, openTicketMeta, false) : null}
            onOpen={(ticket) => setOpenTicketId(ticket.id)}
            onBack={() => setOpenTicketId(null)}
            onNewTicket={() => open({ kind: "ticket" })}
            workspaces={workspaces}
            onOpenWorkspace={(w) => window.open(workspaceHomeUrl(w), "_blank", "noopener,noreferrer")}
            onCreateClone={(ticket) => open({ kind: "clone", ticketUrl: ticket.url, columnId: null })}
            onOpenInLinear={(url) => window.open(url, "_blank", "noopener,noreferrer")}
            onCopy={copyText}
            onMove={(ticket, move) => applyTicketOrder(moved(visibleTickets.map((x) => x.id), ticket.id, move))}
            onMoveToBacklog={(ticket) => moveTicket(ticket, "backlog")}
            onCancel={(ticket) => moveTicket(ticket, "canceled")}
          />
        );
      case "clones":
        return (
          <ClonesView
            columnsLayout={wide}
            columns={columns}
            clones={state.hosts}
            cloneTickets={liveCloneTickets}
            stats={stats}
            cloneTokens={state.cloneTokens ?? {}}
            operations={state.operations}
            mutedClones={mutedClones}
            selectedId={state.selected ?? null}
            notesId={notesId}
            onSelect={selectClone}
            onOpen={(clone) => {
              selectClone(clone);
              // An archived one has no screen: its notes stay open
              if (!clone.archived) onTab(null);
            }}
            onNewClone={(columnId) => open({ kind: "clone", ticketUrl: "", columnId })}
            onMoveToColumn={(clone, columnId) => moveToColumn(clone, columnId)}
            onRenameColumn={(id, title) => applyColumns(columns.map((c) => (c.id === id ? { ...c, title } : c)))}
            onMove={moveInColumn}
            onOpenInLinear={(url) => window.open(url, "_blank", "noopener,noreferrer")}
            onChangeAccount={(clone) => open({ kind: "account", clone })}
            onRebase={(clone) => open({ kind: "rebase", clone })}
            onPortForward={(clone) => open({ kind: "forward", clone })}
            onCopySsh={(clone) => copyText(buildSshCommand(sshHost, BASTION_PORT, clone.id))}
            onToggleMute={(clone) => {
              const next = toggleMuted(mutedClones, clone.id);
              setMutedClones(next);
              run(putMutedClones(next));
            }}
            onArchive={(clone) => run(archiveClone(clone.id))}
            onUnarchive={(clone) => run(unarchiveClone(clone.id))}
            onDelete={deleteOne}
          />
        );
      case "notes":
        return notesView();
      case "settings":
        return <SettingsView {...settings} />;
    }
  };

  if (width >= WIDE_MIN) {
    return (
      // Fixed things in it (a sheet, a dialog) take its whole space: the pages' box over the
      // screen, or the window
      <ModalPlacementProvider value="window">
        <div className={`flex h-full select-text flex-col gap-2 [&_button]:select-none ${glass ? "" : "p-2"}`}>
          {error ? (
            <div className={glass ? `mx-2 shrink-0 overflow-hidden ${PAGES_GLASS}` : "shrink-0 overflow-hidden rounded-xl"}>
              <ErrorLine message={error} onDismiss={() => setError(null)} />
            </div>
          ) : null}
          <div className="min-h-0 flex-1">
            <WideLayout
              glass={glass}
              usage={usage}
              settingsOpen={tab === "settings"}
              onSettings={() => onTab(tab === "settings" ? "clones" : "settings")}
              onPopOut={onPopOut}
              onLayout={onLayout}
              rail={page("usage", true)}
              board={
                // One drag area for both: a ticket dropped on a clone column makes a clone
                <BoardDnd
                  lanes={lanes}
                  tickets={visibleTickets.map((x) => x.id)}
                  onMoveCard={dropClone}
                  onReorderTickets={applyTicketOrder}
                  onTicketToColumn={dropTicketOnColumn}
                >
                  {page("tickets", true)}
                  {page("clones", true)}
                </BoardDnd>
              }
              // An open ticket takes the side panel, as on the dashboard; the clone's notes come
              // back when it closes, and when the pages close (the side panels without the pages
              // are the clone's)
              side={{
                ticket: openTicket && tab ? page("tickets") : undefined,
                notes: notesView("notes"),
                chat: notesView("chat"),
              }}
              sideAt={sideAt}
              settings={page("settings")}
            />
          </div>
          {dialog ? dialogView(dialog.d) : null}
        </div>
      </ModalPlacementProvider>
    );
  }

  return (
    <ModalPlacementProvider value="inline">
      <NarrowLayout row={{ width: width - 8, usage, tab, onTab, badges, onPopOut }}>
        {TABS.filter((t) => visited.has(t) || t === tab).map((t) => (
            // Text can be selected and typed on the pages, not on the keys (WebKit inherits
            // `user-select`); a long press on a button still selects nothing
            <div
              key={t}
              role="tabpanel"
              aria-label={TAB_INFO.find((i) => i.tab === t)?.page}
              hidden={t !== tab}
              className="absolute inset-0 flex select-text flex-col overflow-hidden [&_button]:select-none"
            >
              {error ? <ErrorLine message={error} onDismiss={() => setError(null)} /> : null}
              <div className="relative min-h-0 flex-1">{withDnd(t, page(t))}</div>
              {dialog?.on === t ? dialogView(dialog.d) : null}
            </div>
          ))}
      </NarrowLayout>
    </ModalPlacementProvider>
  );
}
