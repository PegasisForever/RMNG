// The control panel's container: it holds what its tabs draw (the live server state, the
// Linear tickets), does what they ask, and opens the dialogs. The board's columns, the ticket
// order and the mutes are the board module's (`./useBoard.ts`), the tickets the ticket
// module's (`~/lib/linear/useTickets.ts`). Each write has the same shape: show the change
// now, send it, put the old value back if it is refused.
//
// Narrow, the control panel is tabs under the tool row (`../ToolRow.tsx`). Each tab, once opened,
// stays mounted while another one shows, so a half-typed setting, a scrolled list or an open
// ticket is still there when the operator comes back. A dialog opens in the tab that opened
// it, in that tab's space (`ModalPlacementProvider`), so the clone's screen stays in view.
//
// Wide (`WIDE_MIN`), they are columns (`./WideLayout.tsx`): the usage rail, the
// board's columns, the side panel; a dialog opens over them, in the middle. Over the clone's
// screen each column has a pin: a pinned column stays in this monitor window's dock, at its left
// edge, while the control panel is closed (`dock`, `~/lib/dock.ts`). A dialog a docked column
// opens shows by itself, over the clone's screen; what needs the control panel (Settings, an
// open ticket) opens it.
import { lazy, type ReactNode, Suspense, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useSearchParams } from "react-router";

import { ChangeAccountModalContainer } from "~/components/ChangeAccountModalContainer";
import { CloneModalContainer } from "~/components/CloneModalContainer";
import { ForkModalContainer } from "~/components/ForkModalContainer";
import { ask } from "~/components/ConfirmLayer";
import { ImportAccountModalContainer } from "~/components/ImportAccountModalContainer";
import { ModalPlacementProvider, ModalSurfaceProvider } from "~/components/ModalShell";
import { PortForwardModal } from "~/components/PortForwardModal";
import { RebaseModalContainer } from "~/components/RebaseModalContainer";
import { useSettingsPanel } from "~/components/useSettingsPanel";
import { TicketModalContainer } from "~/components/TicketModalContainer";
import { TicketPanel } from "~/components/TicketPanel";
import { useAccountOrder } from "~/lib/accountOrder";
import { rememberSideWidth, SIDE_DEFAULT, storedSideWidth } from "~/lib/sidePanelWidth";
import {
  activate,
  archiveClone,
  deleteClone,
  rebaseClone,
  renameClone,
  refreshClaudeUsage,
  refreshCodexUsage,
  startClone,
  unarchiveClone,
} from "~/lib/api";
import { columnIdOf, resolveColumns, withDefaults } from "~/lib/board";
import { copyText } from "~/lib/clipboard";
import { cloneTitle } from "~/lib/cloneTitle";
import { columnPin, RAIL_PIN, TICKETS_PIN } from "~/lib/dock";
import { useTickets } from "~/lib/linear/useTickets";
import { workspaceHomeUrl } from "~/lib/linear/workspaces";
import { focusPage, hostWindow } from "~/lib/host";
import { readSelection, sameSelection, type Selection, withSelection } from "~/lib/selection";
import { BASTION_PORT, buildSshCommand } from "~/lib/ssh";
import { findTicket, type LinearTicket } from "~/lib/tickets";
import type { ClaudeUsage, Clone, ControlState } from "~/lib/types";
import { useCloneNotifications } from "~/lib/useCloneNotifications";
import { useLeftJobs } from "~/lib/useLeftJobs";
import { useNow } from "~/lib/useNow";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { SoundReport } from "~/viewer/protocol";

import { CloneAlerts } from "../CloneAlerts";
import { type PanelTab, ROW_STATS_MIN, type ServerUsage, TABS as TAB_INFO } from "../ToolRow";
import { ClonesView } from "./ClonesView";
import { NotesView, type NotesPane } from "./NotesView";
import { moved } from "./order";
import { SettingsView } from "./SettingsView";
import { TicketsView } from "./TicketsView";
import { COLUMN_GLASS, type ColumnPins, ColumnPinsProvider, ErrorLine, TEXT_3 } from "./ui";
import { BoardDnd } from "./BoardDnd";
import { NarrowLayout } from "./NarrowLayout";
import { useBoard } from "./useBoard";
import { UsageView } from "./UsageView";
import { DockLayout, RailColumn, WIDE_MIN, WideLayout } from "./WideLayout";

// Browser-only and heavy (BlockNote, the chat stream): loaded when first shown
const NotesEditorContainer = lazy(() => import("~/components/NotesEditorContainer"));
const ChatContainer = lazy(() => import("~/components/ChatContainer"));
const TicketDescription = lazy(() => import("~/components/TicketDescription"));

const TABS: PanelTab[] = ["usage", "tickets", "clones", "notes", "assistant", "settings"];

function Loading({ label }: { label: string }) {
  return <p className="p-4 text-sm text-black/40 dark:text-white/40">{label}</p>;
}

/** A dialog that is open, and the tab it opened in (null: a pinned column opened it while the
 *  control panel was closed). */
type Dialog =
  | { kind: "clone"; ticketUrl: string; columnId: string | null }
  | { kind: "ticket" }
  | { kind: "import"; replacing: ClaudeUsage | null; provider?: "claude" | "codex"; group?: string }
  | { kind: "account"; clone: Clone }
  | { kind: "rebase"; clone: Clone }
  | { kind: "fork"; clone: Clone }
  | { kind: "forward"; clone: Clone };

export function ControlPanelContainer({
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
  notify = false,
  alertsAt,
  dock,
}: {
  /** The control panel's width, CSS px: narrow (tabs) or wide (columns). */
  width: number;
  /** Over the clone's screen: wide, each column is glass of its own (narrow, the screen's view
   *  puts the whole control panel on one glass panel). Not in the control panel's own window. */
  glass: boolean;
  /** The server's CPU, memory and disk; null until they arrive. */
  usage: ServerUsage | null;
  /** A count on a tab. */
  badges?: Partial<Record<PanelTab, number>>;
  /** Move the control panel to a window of its own; absent where the browser opens none. */
  onPopOut?: () => void;
  /** The tab that shows; null while the keyboard shows. */
  tab: PanelTab | null;
  /** Show another tab, or the keyboard (null). */
  onTab: (tab: PanelTab | null) => void;
  state: ControlState;
  stats: Record<string, ContainerStats>;
  /** How the clone's sound plays in this window; null before it plays (or where none does). */
  sound: SoundReport | null;
  /** Wide, over the clone's screen: the screen's box for the side panels, which shows them
   *  without the control panel too (`./WideLayout.tsx`); null until it is there. */
  sideAt?: HTMLElement | null;
  /** Open the Edit layout dialog; absent where there is none (the control panel's own window). */
  onLayout?: () => void;
  /** This window raises the notifications of clones that stop working (one window of the
   *  browser does: monitor 0's). */
  notify?: boolean;
  /** Over the clone's screen, in the window that notifies: the screen's box for the alerts of
   *  clones that stopped working (`../CloneAlerts.tsx`); null until it is there. */
  alertsAt?: HTMLElement | null;
  /** Over the clone's screen: this monitor window's pinned columns (pinned from the wide
   *  columns; the dock stays where the control panel is narrow). `pins`: the pins of
   *  columns there are, in the board's order (`orderPins`); `docked`: those the dock has room
   *  for (`fitPins`); `onToggle`: a press on a column's
   *  pin; `at`: the dock's box at the window's left edge (null until it is there, or while no
   *  column is pinned); `onShowPanel`: open the control panel (on `tab`), for what a docked
   *  column does that needs it (Settings, an open ticket); `onDialog`: a dialog shows or goes. Absent: no pins (the control panel's own window). */
  dock?: {
    pins: readonly string[];
    docked: readonly string[];
    onToggle: (pin: string) => void;
    at: HTMLElement | null;
    onShowPanel: (tab?: PanelTab) => void;
    /** A dialog is on the page that shows with the control panel closed (or none any more). */
    onDialog?: (open: boolean) => void;
    /** A clone chosen in a docked column is on the screen now: the keys go to it. */
    onCloneShown?: () => void;
  };
}) {
  const [error, setError] = useState<string | null>(null);
  const run = (p: Promise<unknown>) => p.then(() => setError(null)).catch((e: Error) => setError(e.message));
  const { acctOrder } = useAccountOrder();
  const now = useNow();
  const sshHost = typeof window === "undefined" ? "" : window.location.hostname;

  // Wide: the side panels' width (one stored width for both), read after mount
  const [sideWidth, setSideWidth] = useState(SIDE_DEFAULT);
  useEffect(() => {
    const stored = storedSideWidth();
    if (stored !== null) setSideWidth(stored);
  }, []);

  // The tabs opened so far stay mounted (see the top of this file)
  const [visited, setVisited] = useState<ReadonlySet<PanelTab>>(new Set());
  useEffect(() => {
    if (tab && !visited.has(tab)) setVisited(new Set([...visited, tab]));
  }, [tab, visited]);

  const [dialog, setDialog] = useState<{ on: PanelTab | null; d: Dialog } | null>(null);
  const wide = width >= WIDE_MIN;
  /** Open a dialog; from a pinned column while the control panel is closed, by itself over the
   *  clone's screen (the control panel stays closed). */
  const open = (d: Dialog) => {
    if (tab) return setDialog({ on: tab, d });
    if (dock) setDialog({ on: null, d });
  };
  // Where a dialog shows with the control panel closed (one from a pinned column; wide, any:
  // a window dialog is on the page, not in the control panel's box), the page keeps the mouse
  // and the keys from the clone
  const onDialog = dock?.onDialog;
  useEffect(() => {
    onDialog?.(dialog !== null && (dialog.on === null || wide));
  }, [dialog, wide, onDialog]);
  const closeDialog = () => setDialog(null);
  /** The dialog open now, for a job that answers after its dialog closed. */
  const openDialog = useRef(dialog);
  openDialog.current = dialog;
  // A dialog closed while its job runs leaves the job here: a failure shows on the error line
  const leave = useLeftJobs(state.operations, setError);

  const clonesById = new Map(state.hosts.map((h) => [h.id, h]));
  const accounts = state.claudeAccounts ?? [];

  // ---- the board's columns, the ticket order and the mutes (`./useBoard.ts`) ----
  const board = useBoard(
    {
      clones: state.hosts,
      columns: state.boardColumns ?? [],
      ticketOrder: state.ticketOrder ?? [],
      muted: state.mutedClones ?? [],
    },
    { onError: setError },
  );

  // What is open, held in the page's address (`?clone=`, `?ticket=`;
  // `~/lib/selection`): the browser's Back returns to the one before it, and a link opens it
  const [params, setParams] = useSearchParams();
  const selection = readSelection(params);
  /** Open something, with a history entry for Back; `replace`: in place of the current one, for
   *  a move the operator did not ask for by name. */
  const select = (next: Selection, replace = false) => {
    if (sameSelection(next, selection)) return;
    setParams((p) => withSelection(p, next), { replace, preventScrollReset: true });
  };
  const setOpenTicketId = (ticket: string | null, replace = false) => select({ ...selection, ticket }, replace);

  // ---- Linear (`~/lib/linear/useTickets.ts`) ----
  const tickets = useTickets({
    clones: state.hosts,
    order: board.ticketOrder,
    openId: selection.ticket,
    onOpen: setOpenTicketId,
    onShowClone: (clone) => selectClone(clone),
    onError: setError,
  });
  const visibleTickets = tickets.visible;
  const openTicket = tickets.open;
  // Each ticket keeps the place it was first drawn in
  useEffect(() => board.adoptTickets(visibleTickets));

  // ---- the clone on the screen, and the one the Notes tab is on ----
  /** An archived clone the Notes tab shows; null: the one on the screen. */
  const addressed = selection.clone ? (clonesById.get(selection.clone) ?? null) : null;
  const notesId = addressed?.archived ? addressed.id : null;
  const notesClone = (notesId ? clonesById.get(notesId) : null) ?? (state.selected ? clonesById.get(state.selected) : null) ?? null;
  const notesTicket = notesClone?.linearTicket ? tickets.find(notesClone.linearTicket) : null;

  /** Put a clone on the screen; the control panel stays open (the operator closes it). An archived one
   *  has no screen, so its notes open instead. Wide, an open ticket closes:
   *  the side panels are the clone's again. */
  const selectClone = (clone: Clone) => {
    select({ clone: clone.id, ticket: wide ? null : selection.ticket });
    if (clone.archived) {
      onTab("notes");
      return;
    }
    applied.current = clone.id;
    if (state.selected !== clone.id) run(activate(clone.id));
  };

  // A live clone the address names (Back, Forward, a reload, a link) goes on the screen, as a
  // press on it does: once for each value, so it does not fight a choice made elsewhere
  const applied = useRef<string | null>(null);
  useEffect(() => {
    const id = selection.clone;
    if (applied.current === id) return;
    if (id === null) {
      applied.current = null;
      return;
    }
    // Not in the list yet (the first frame has not come) or gone: the next frame tries again
    const clone = clonesById.get(id);
    if (!clone) return;
    applied.current = id;
    if (!clone.archived && state.selected !== id) run(activate(id));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection.clone, state.hosts]);
  // A clone chosen elsewhere (another window, the phone, `rmng clone select`) goes into the
  // address in place of the one there, so a reload (a new server build reloads the page) does
  // not put that one back on the screen. Archived notes the operator has open stay.
  useEffect(() => {
    const id = state.selected;
    if (!id || notesId || selection.clone === id) return;
    applied.current = id;
    select({ ...selection, clone: id }, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.selected]);

  // A clone that stops working raises a notification (in the page, or the system's); a press on
  // it shows that clone
  const alerts = useCloneNotifications(
    state.hosts,
    board.mutedClones,
    (id) => {
      focusPage();
      const clone = clonesById.get(id);
      if (clone) selectClone(clone);
    },
    notify,
  );

  /** A ticket's panel; `own`: a clone's own ticket, in its Notes. */
  const ticketPanel = (t: LinearTicket, own: boolean) => (
    <TicketPanel
      {...tickets.panel(t, own)}
      onCopyBranchName={copyText}
      onCopyTicketLink={copyText}
      description={
        <Suspense fallback={<Loading label="Loading…" />}>
          <TicketDescription key={t.id} markdown={t.description ?? ""} onSave={(md) => tickets.edit(t, { description: md })} />
        </Suspense>
      }
      onCreateClone={own ? undefined : () => open({ kind: "clone", ticketUrl: t.url, columnId: null })}
    />
  );

  const notesSlot = () => {
    if (!notesClone) return null;
    if (notesClone.linearTicket && notesTicket) return ticketPanel(notesTicket, true);
    const editor = (
      <Suspense fallback={<Loading label="Loading editor…" />}>
        <NotesEditorContainer key={notesClone.id} cloneId={notesClone.id} />
      </Suspense>
    );
    if (!notesClone.linearTicket) return editor;
    if (tickets.loading) return <Loading label={`Loading ${notesClone.linearTicket}…`} />;
    return (
      <>
        <p className={`px-4 pb-2 text-xs ${TEXT_3}`}>
          {notesClone.linearTicket} did not load{tickets.error ? `: ${tickets.error}` : ""}. Showing notes.
        </p>
        {editor}
      </>
    );
  };

  // ---- clones ----
  const deleteOne = (clone: Clone) => {
    const subs = state.hosts.filter((h) => h.parent === clone.id).length;
    const also = subs > 0 ? ` and its ${subs} sub clone${subs === 1 ? "" : "s"}` : "";
    const q = clone.managed
      ? { title: `Delete ${clone.id}${also}?`, body: `This destroys its container${subs > 0 ? "s" : ""}.`, ok: "Delete" }
      : { title: `Remove ${clone.id}?`, body: "This unregisters the clone.", ok: "Remove" };
    void ask({ ...q, danger: true }).then((yes) => {
      if (yes) run(deleteClone(clone.id));
    });
  };

  // The clone just made goes on the screen when its dialog closes
  const newClone = useRef<string | null>(null);

  // ---- settings ----
  const settings = useSettingsPanel({
    accounts,
    operations: state.operations,
    onImportAccount: (provider, group) => open({ kind: "import", replacing: null, provider, group }),
    forkSources: state.hosts.filter((h) => h.managed).map((h) => h.id),
    // A Linear key typed into Settings is asked with at once
    onSaved: (config) => tickets.setPresets(config.presets),
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
            onClose={(left) => {
              closeDialog();
              const made = newClone.current ? clonesById.get(newClone.current) : null;
              newClone.current = null;
              // Closed while it was being made: the operator went on, the screen stays
              if (made && !left) selectClone(made);
            }}
            onStart={(fork, req) =>
              startClone(fork, req).then((op) => {
                if (d.columnId) board.fileNewClone(op.target, d.columnId);
                // Only while its dialog is open: once it closed, no dialog's close shows it
                if (openDialog.current?.d === d) newClone.current = op.target;
                return op;
              })
            }
            onLeave={leave}
          />
        );
      case "ticket":
        return (
          <TicketModalContainer presets={tickets.presets} onClose={closeDialog} onCreate={tickets.create} onLeave={leave} />
        );
      case "import":
        return (
          <ImportAccountModalContainer
            accounts={accounts}
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
            groups={state.groups}
            onClose={closeDialog}
            onLeave={leave}
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
            onLeave={leave}
          />
        );
      case "fork": {
        const ticket = tickets.forClones[d.clone.id];
        return (
          <ForkModalContainer
            clone={d.clone}
            ticketTitle={d.clone.linearTicket ? cloneTitle(d.clone, ticket) : null}
            operations={state.operations}
            accounts={accounts}
            onClose={(left) => {
              closeDialog();
              const made = newClone.current ? clonesById.get(newClone.current) : null;
              newClone.current = null;
              // Closed while it was being made: the operator went on, the screen stays
              if (made && !left) selectClone(made);
            }}
            onFork={(req) =>
              startClone(true, req).then((op) => {
                // Not a sub clone: it goes in the column of its source (or of the source's
                // parent). A sub clone shows under its parent and has no column.
                if (!req.parent) {
                  const columns = resolveColumns(withDefaults(board.columns), state.hosts);
                  const columnId = columnIdOf(columns, d.clone.parent ?? d.clone.id);
                  if (columnId) board.fileNewClone(op.target, columnId);
                }
                if (openDialog.current?.d === d) newClone.current = op.target;
                return op;
              })
            }
            onLeave={leave}
          />
        );
      }
      case "forward":
        return <PortForwardModal clone={d.clone} sshPublicHost={sshHost} onClose={closeDialog} onCopy={copyText} />;
    }
  };
  // ---- drag and drop (`./BoardDnd.tsx`) ----
  const dropClone = (cloneId: string, columnId: string, index: number) => {
    if (clonesById.has(cloneId)) board.moveClone(cloneId, columnId, index);
  };
  const dropTicketOnColumn = (ticketId: string, columnId: string) => {
    const t = findTicket(ticketId, visibleTickets);
    if (t) open({ kind: "clone", ticketUrl: t.url, columnId });
  };
  /** A tab with what drags in it: the clones between their columns, the tickets in order. */
  const withDnd = (t: PanelTab, node: ReactNode): ReactNode =>
    t === "clones" ? (
      <BoardDnd lanes={board.lanes} onMoveCard={dropClone}>
        {node}
      </BoardDnd>
    ) : t === "tickets" ? (
      <BoardDnd tickets={visibleTickets.map((x) => x.id)} onReorderTickets={board.orderTickets}>
        {node}
      </BoardDnd>
    ) : (
      node
    );

  /** The Notes or the Assistant tab (narrow), or (`card`, wide) its side panel. */
  const notesView = (pane: NotesPane, card = false) => (
    <NotesView
      only={card ? pane : undefined}
      clone={notesClone}
      ticket={notesClone ? tickets.forClones[notesClone.id] : undefined}
      pane={pane}
      notesLabel={notesClone?.linearTicket ? "Ticket" : "Notes"}
      notes={notesSlot()}
      // Wide, the clone's own ticket fills its card (`notesSlot` gives it then)
      ticketCard={card && pane === "notes" && notesClone?.linearTicket && notesTicket ? notesSlot() : undefined}
      chat={
        notesClone ? (
          <Suspense fallback={<Loading label="Loading chat…" />}>
            <ChatContainer key={notesClone.id} cloneId={notesClone.id} archived={notesClone.archived === true} />
          </Suspense>
        ) : null
      }
      onBackToScreen={notesId && notesId !== state.selected ? () => select({ ...selection, clone: state.selected }) : undefined}
    />
  );

  /** A tab's content, as the tabs show it, or (`wide`) as its column; `docked`: in the pinned
   *  columns' dock, the clone columns `docked.columns` only. */
  const tabContent = (t: PanelTab, wide = false, docked: { columns: readonly string[] } | null = null): ReactNode => {
    switch (t) {
      case "usage":
        return (
          <UsageView
            rail={wide}
            server={wide || width - 12 >= ROW_STATS_MIN ? undefined : usage}
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
            loading={tickets.loading}
            error={tickets.error}
            open={openTicket}
            detail={openTicket ? ticketPanel(openTicket, false) : null}
            onOpen={(ticket) => {
              setOpenTicketId(ticket.id);
              // The open ticket shows with the control panel: in the side panels, or (narrow) its tab
              if (docked && !tab) dock?.onShowPanel("tickets");
            }}
            onBack={() => setOpenTicketId(null)}
            onNewTicket={() => open({ kind: "ticket" })}
            workspaces={tickets.workspaces}
            onOpenWorkspace={(w) => window.open(workspaceHomeUrl(w), "_blank", "noopener,noreferrer")}
            onCreateClone={(ticket) => open({ kind: "clone", ticketUrl: ticket.url, columnId: null })}
            onOpenInLinear={(url) => window.open(url, "_blank", "noopener,noreferrer")}
            onCopy={copyText}
            onMove={(ticket, move) => board.orderTickets(moved(visibleTickets.map((x) => x.id), ticket.id, move))}
            onMoveToBacklog={(ticket) => tickets.moveTo(ticket, "backlog")}
            onCancel={(ticket) => tickets.moveTo(ticket, "canceled")}
          />
        );
      case "clones":
        return (
          <ClonesView
            columnsLayout={wide}
            onlyColumns={docked?.columns}
            columns={board.columns}
            clones={state.hosts}
            cloneTickets={tickets.forClones}
            stats={stats}
            cloneTokens={state.cloneTokens ?? {}}
            operations={state.operations}
            mutedClones={board.mutedClones}
            selectedId={state.selected ?? null}
            notesId={notesId}
            onSelect={(clone) => {
              selectClone(clone);
              if (docked && !clone.archived) dock?.onCloneShown?.();
            }}
            onRename={(clone, name) => run(renameClone(clone.id, name))}
            onOpen={(clone) => {
              selectClone(clone);
              // An archived one has no screen: its notes stay open
              if (!clone.archived) onTab(null);
              if (docked && !clone.archived) dock?.onCloneShown?.();
            }}
            onNewClone={(columnId) => open({ kind: "clone", ticketUrl: "", columnId })}
            onMoveToColumn={(clone, columnId) => board.moveClone(clone.id, columnId)}
            onRenameColumn={board.renameColumn}
            onMove={(clone, columnId, move) => board.moveInColumn(clone.id, columnId, move)}
            onOpenInLinear={(url) => window.open(url, "_blank", "noopener,noreferrer")}
            onChangeAccount={(clone) => open({ kind: "account", clone })}
            onRebase={(clone) => open({ kind: "rebase", clone })}
            onFork={(clone) => open({ kind: "fork", clone })}
            onPortForward={(clone) => open({ kind: "forward", clone })}
            onCopySsh={(clone) => copyText(buildSshCommand(sshHost, BASTION_PORT, clone.id))}
            onToggleMute={(clone) => board.toggleMute(clone.id)}
            onArchive={(clone) => run(archiveClone(clone.id))}
            onUnarchive={(clone) => run(unarchiveClone(clone.id))}
            onDelete={deleteOne}
          />
        );
      case "notes":
        return notesView("notes");
      case "assistant":
        return notesView("chat");
      case "settings":
        // The test page in the browser's own window: the app's frame has no safe area of its own
        return <SettingsView {...settings} {...board.settings} onScreenArea={() => hostWindow().location.assign("/screen-area")} />;
    }
  };

  // On the clone's screen, not in the control panel: they show while it is closed too
  const alertsView = alertsAt
    ? createPortal(<CloneAlerts alerts={alerts.alerts} onOpen={alerts.open} onClose={alerts.close} />, alertsAt)
    : null;

  // The pins (a press on one pins or unpins its column), and the pinned columns in the dock. The
  // dock stays where the control panel is narrow too: the pins are the window's, not its width's
  const pins: ColumnPins = dock ? { pinned: new Set(dock.pins), onToggle: dock.onToggle } : null;
  const dockColumns = (dock?.docked ?? []).filter((p) => p.startsWith(columnPin(""))).map((p) => p.slice(columnPin("").length));
  const dockView =
    dock?.at && dock.docked.length
      ? createPortal(
          // No clone's screen under the dock: its menus and dialogs solid, as the control panel's
          // window has them
          <ModalSurfaceProvider value="solid">
            <ColumnPinsProvider value={pins}>
              <DockLayout>
                {dock.docked.includes(RAIL_PIN) ? (
                  <RailColumn
                    glass={false}
                    usage={usage}
                    settingsOpen={false}
                    onSettings={() => dock.onShowPanel("settings")}
                    onPopOut={onPopOut}
                    onLayout={onLayout}
                    rail={tabContent("usage", true)}
                  />
                ) : null}
                {/* A drag area of its own, as the board's: the clones between the docked columns,
                    the tickets in order, a ticket onto a docked column */}
                <BoardDnd
                  lanes={board.lanes}
                  tickets={visibleTickets.map((x) => x.id)}
                  onMoveCard={dropClone}
                  onReorderTickets={board.orderTickets}
                  onTicketToColumn={dropTicketOnColumn}
                >
                  {dock.docked.includes(TICKETS_PIN) ? tabContent("tickets", true, { columns: [] }) : null}
                  {dockColumns.length ? tabContent("clones", true, { columns: dockColumns }) : null}
                </BoardDnd>
              </DockLayout>
            </ColumnPinsProvider>
          </ModalSurfaceProvider>,
          dock.at,
        )
      : null;

  /** A pinned column's dialog: over the whole page (the narrow control panel's tabs put theirs
   *  in the tab's space), with the control panel closed. */
  const dockDialog =
    dialog && dialog.on === null ? <ModalPlacementProvider value="window">{dialogView(dialog.d)}</ModalPlacementProvider> : null;

  if (wide) {
    return (
      // Fixed things in it (a sheet, a dialog) take its whole space: the control panel's box over the
      // screen, or the window
      <ModalPlacementProvider value="window">
        <div className={`flex h-full select-text flex-col gap-2 [&_button]:select-none ${glass ? "" : "p-2"}`}>
          {error ? (
            <div className={glass ? `mx-2 shrink-0 ${COLUMN_GLASS}` : "shrink-0 overflow-hidden rounded-xl"}>
              <ErrorLine message={error} onDismiss={() => setError(null)} />
            </div>
          ) : null}
          <div className="min-h-0 flex-1">
            <ColumnPinsProvider value={pins}>
              <WideLayout
                width={width}
                glass={glass}
                usage={usage}
                settingsOpen={tab === "settings"}
                onSettings={() => onTab(tab === "settings" ? "clones" : "settings")}
                onPopOut={onPopOut}
                onLayout={onLayout}
                rail={tabContent("usage", true)}
                board={
                  // One drag area for both: a ticket dropped on a clone column makes a clone
                  <BoardDnd
                    lanes={board.lanes}
                    tickets={visibleTickets.map((x) => x.id)}
                    onMoveCard={dropClone}
                    onReorderTickets={board.orderTickets}
                    onTicketToColumn={dropTicketOnColumn}
                  >
                    {tabContent("tickets", true)}
                    {tabContent("clones", true)}
                  </BoardDnd>
                }
                // An open ticket takes the side panel; the clone's notes come
                // back when it closes, and when the control panel closes (the side panels without the control panel
                // are the clone's)
                side={{
                  ticket: openTicket && tab ? ticketPanel(openTicket, false) : undefined,
                  notes: notesClone ? notesView("notes", true) : null,
                  chat: notesView("chat", true),
                  widthPct: sideWidth,
                }}
                sideAt={sideAt}
                onSideWidthCommit={(pct) => {
                  setSideWidth(pct);
                  rememberSideWidth(pct);
                }}
                settings={tabContent("settings")}
              />
            </ColumnPinsProvider>
          </div>
          {dialog?.on ? dialogView(dialog.d) : null}
          {alertsView}
          {dockView}
          {dockDialog}
        </div>
      </ModalPlacementProvider>
    );
  }

  return (
    <ModalPlacementProvider value="inline">
      <NarrowLayout row={{ width: width - 12, usage, tab, onTab, badges, onPopOut }}>
        {TABS.filter((t) => visited.has(t) || t === tab).map((t) => (
            // Text can be selected and typed on the control panel, not on the keys (WebKit inherits
            // `user-select`); a long press on a button still selects nothing
            <div
              key={t}
              role="tabpanel"
              aria-label={TAB_INFO.find((i) => i.tab === t)?.title}
              hidden={t !== tab}
              className="absolute inset-0 flex select-text flex-col overflow-hidden [&_button]:select-none"
            >
              {error ? <ErrorLine message={error} onDismiss={() => setError(null)} /> : null}
              <div className="relative min-h-0 flex-1">{withDnd(t, tabContent(t))}</div>
              {dialog?.on === t ? dialogView(dialog.d) : null}
            </div>
          ))}
      </NarrowLayout>
      {alertsView}
      {dockView}
      {dockDialog}
    </ModalPlacementProvider>
  );
}
