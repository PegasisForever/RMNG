// The desktop dashboard's network half: it owns every mutation the board can start, the
// dialogs those mutations open, and the browser reads the shell must not make.
//
// It is the counterpart of `MobileDashboardContainer`, not a wrapper around it. The two share
// the live state and the config gate the route holds above both, and nothing else: the board,
// its drag handlers, and every dialog except account import stay on this side.
//
// The shell it renders is pure, so three things it cannot do are resolved here and handed
// down: the operator's locale, the page's own address (the SSH command's jump target when no
// public host is configured), and the remembered side-panel width.
//
// Dialogs are mounted beside the shell rather than inside it. Each one positions itself, so
// the shell would only be passing them through, and a page story then shows the page with
// nothing on top of it while each dialog's own stories cover every state it has.
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import { AppShellV2, type SideFocus } from "~/components/AppShellV2";
import { ChangeAccountModalContainer } from "~/components/ChangeAccountModalContainer";
import { CloneModalContainer } from "~/components/CloneModalContainer";
import { CommitImageModal } from "~/components/CommitImageModal";
import { ImportAccountModalContainer } from "~/components/ImportAccountModalContainer";
import { PortForwardModal } from "~/components/PortForwardModal";
import { SettingsPanelContainer } from "~/components/SettingsPanelContainer";
import { TicketModalContainer } from "~/components/TicketModalContainer";
import { TicketPanel } from "~/components/TicketPanel";
import {
  issueSetLabel,
  issueSetState,
  issueSetStateId,
  issueUpdate,
  keysForTeam,
} from "~/lib/linear/mutations";
import type { TicketLabel, TicketWorkflowState } from "~/lib/linear/types";
import { useTeamMeta } from "~/lib/linear/useTeamMeta";
import { useTickets } from "~/lib/linear/useTickets";
import { useWorkspaces } from "~/lib/linear/useWorkspaces";
import { workspaceHomeUrl } from "~/lib/linear/workspaces";
import {
  adoptTickets,
  cloneForTicket,
  cloneTickets,
  findTicket,
  openTickets,
  orderTickets,
  queued,
  ticketAfter,
  type LinearTicket,
  type TicketState,
} from "~/lib/tickets";
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
  putMutedClones,
  putTicketOrder,
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
import { useAccountOrder } from "~/lib/accountOrder";
import { copyText } from "~/lib/clipboard";
import { browserLocale } from "~/lib/format";
import { readSelection, sameSelection, withSelection, type Selection } from "~/lib/selection";
import { rememberSideWidth, SIDE_DEFAULT, storedSideWidth } from "~/lib/sidePanelWidth";
import { type ControlState, type Clone } from "~/lib/types";
import { toggleMuted } from "~/lib/mute";
import { useCloneNotifications } from "~/lib/useCloneNotifications";
import { useNow } from "~/lib/useNow";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import type { LxcStats } from "~/lib/wire/LxcStats";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

// BlockNote + the chat panel are browser-only; load them lazily and render only
// after mount so they never participate in SSR.
const NotesEditorContainer = lazy(() => import("~/components/NotesEditorContainer"));
const ChatContainer = lazy(() => import("~/components/ChatContainer"));
// The ticket description is markdown, and rendering it means BlockNote, which is as
// browser-only as the notes editor that already uses it.
const TicketDescription = lazy(() => import("~/components/TicketDescription"));

function ClientOnly({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  return mounted ? <>{children}</> : null;
}

export function DashboardContainer({
  state,
  stats,
  lxcStats,
  forwards,
  sshPublicHost,
  bastionPort,
  cloneGroups,
  codexGroups,
  presets,
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
  /** Configured Claude pools (`config.cloneGroups`) — the rail's usage list groups by these. */
  cloneGroups: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups: CloneGroup[];
  /** Configured presets (`config.presets`). Their labels are the ticket dialog's team keys. */
  presets: PresetRedacted[];
}) {
  // What the dashboard has open, held in the page's address rather than here, so the browser's
  // own Back button returns to the last thing the operator picked. Two ids, because a ticket
  // panel and a clone are open at the same time: see `~/lib/selection`.
  //
  // Both are ids rather than objects, so a poll that rewrites the ticket list and a frame that
  // rewrites the clone list both keep the panels on the current copy, never a stale one.
  const [params, setParams] = useSearchParams();
  const selection = readSelection(params);

  /** Open something, and leave a history entry behind so Back returns to what was open before.
   *
   *  `replace` rewrites the current entry instead, for a move the operator did not ask for by
   *  name: the entry it would leave points at something that is no longer on the board, and
   *  Back onto it would open nothing. */
  const select = (next: Selection, replace = false) => {
    if (sameSelection(next, selection)) return;
    setParams(withSelection(params, next), { replace, preventScrollReset: true });
  };

  const [error, setError] = useState<string | null>(null);
  const [cloneOpen, setCloneOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // The shared cosmetic account order and the live clock. Both are session reads, so they
  // are resolved here and handed down: the rail's usage bars and the settings account lists
  // then draw from props alone, and both stay in step because they read one subscription.
  const { acctOrder } = useAccountOrder();
  const now = useNow();
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
  // Which half of the side panel gets the height.
  const [sideFocus, setSideFocus] = useState<SideFocus>("notes");
  // The width the operator last left the side panel at. Read after mount rather than in the
  // initial state, so the server and the first client render agree on the default.
  const [sideWidth, setSideWidth] = useState(SIDE_DEFAULT);
  useEffect(() => {
    const stored = storedSideWidth();
    if (stored !== null) setSideWidth(stored);
  }, []);

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
  // An archived clone can be selected, but the viewer cannot follow it: its container is
  // stopped, so there is no stream to show and no input to deliver. So it is a dashboard focus
  // and nothing more, read off the address instead of `state.selected`, which is what the
  // viewer streams. The viewer stays on the last live clone the operator picked.
  //
  // Derived, so a clone that is restored or deleted while its panels are open drops the focus
  // on its own: an id that no longer names an archived clone answers null here, and the panels
  // fall back to whatever the viewer is on, with no cleanup to run.
  const urlClone = selection.clone ? clonesById.get(selection.clone) ?? null : null;
  const openArchived = urlClone?.archived ? urlClone : null;
  const focusedId = openArchived ? openArchived.id : state.selected;
  const selectedClone = focusedId ? clonesById.get(focusedId) ?? null : null;

  // Which live clone the viewer streams is the server's state, not this page's, so an address
  // that names one has to ask for it the way the click that first opened it did. That covers
  // Back, Forward, a reload, and a pasted link alike.
  //
  // Applied once per value, which is what keeps this from fighting the server. A selection
  // that moves for some other reason (a second tab, `rmng clone select`, a notification on
  // another screen) leaves the address a step behind until the next click here, and dragging
  // it back would take the viewer off whatever the operator just went to.
  const appliedClone = useRef<string | null>(null);
  useEffect(() => {
    const id = selection.clone;
    if (appliedClone.current === id) return;
    if (id === null) {
      appliedClone.current = null;
      return;
    }
    // Not in `hosts` yet: either the first frame has not landed or the clone is gone. Leave
    // the ref alone so the next frame gets another try.
    const clone = clonesById.get(id);
    if (!clone) return;
    appliedClone.current = id;
    if (!clone.archived && state.selected !== id) run(activate(id));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection.clone, state.hosts]);

  // The address this page opens on names nothing, and a Back that landed on it would be a step
  // that changed nothing on screen. So the first frame's own selection is written into the
  // current entry rather than a new one, and every entry after it names something.
  const seeded = useRef(false);
  useEffect(() => {
    if (seeded.current) return;
    if (selection.clone || selection.ticket) {
      seeded.current = true; // opened on a link that already names something
      return;
    }
    if (!state.selected) return;
    seeded.current = true;
    select({ clone: state.selected, ticket: null }, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.selected, selection.clone, selection.ticket]);

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

  /** Put a clone in the panels, and close any open ticket, because both want the same column.
   *
   *  Writing the address is the whole of it. The effect above is what activates a live clone
   *  so the viewer follows it, and what leaves an archived one alone: its container is
   *  stopped, so there is no stream to follow and no input to deliver, and the server refuses
   *  to activate it anyway. Every path that lands on a clone goes through this, so the rule is
   *  stated once rather than at each of them. */
  const selectClone = (clone: Clone) => {
    select({ clone: clone.id, ticket: null });
  };

  // The two things a card asks the browser for rather than the server. Both leave the cards
  // themselves pure: a menu item says what it wants done and this decides how.
  //
  // `noopener` is what stops the Linear tab from reaching back through `window.opener`, and
  // `noreferrer` keeps this page's address off the request. Both menus that leave for Linear
  // go through this one call, so neither can drift from the other.
  const openInLinear = (url: string) => {
    window.open(url, "_blank", "noopener,noreferrer");
  };

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
  /** The clone the open create-dialog is making, selected when the dialog closes. */
  const [newClone, setNewClone] = useState<string | null>(null);
  // The new-ticket dialog. Nothing else in the app opens it, so it needs no argument.
  const [newTicketOpen, setNewTicketOpen] = useState(false);
  // Seeds the clone dialog's ticket field when a ticket opened it (dragged onto a column, or
  // its menu). Empty for the column's own "New clone" button.
  const [ticketPrefill, setTicketPrefill] = useState("");
  // The operator's own arrangement of the ticket column, top to bottom. Stored server-side,
  // and held locally on top of that so a drag lands instantly. Same shape as the columns
  // above, for the same reason: keyed on the serialized list rather than the array identity,
  // because every SSE frame brings a fresh array and resetting on each one would undo a drag
  // that is still in flight.
  const serverTicketOrder = JSON.stringify(state.ticketOrder ?? []);
  const [ticketOrder, setTicketOrder] = useState<string[]>(() => state.ticketOrder ?? []);
  useEffect(() => {
    setTicketOrder(JSON.parse(serverTicketOrder) as string[]);
  }, [serverTicketOrder]);

  // Adopt a new order locally, then persist it. The column has already previewed the drag by
  // the time this runs; the PUT is what makes it survive a refresh. The server lowercases
  // what it stores, so the frame that comes back overwrites this with the same order in a
  // different case, which `orderTickets` cannot tell apart.
  const applyTicketOrder = (next: string[]) => {
    setTicketOrder(next);
    run(putTicketOrder(next));
  };

  // The muted-clone set, same optimistic shape as the two above: a mute is a click on a menu
  // that closes itself, so the card has to go quiet in that frame rather than a round trip
  // later. The server sorts what it stores, and `toggleMuted` sorts too, so the frame that
  // comes back is the identical array.
  const serverMuted = JSON.stringify(state.mutedClones ?? []);
  const [mutedClones, setMutedClones] = useState<string[]>(() => state.mutedClones ?? []);
  useEffect(() => {
    setMutedClones(JSON.parse(serverMuted) as string[]);
  }, [serverMuted]);

  const applyMuted = (next: string[]) => {
    setMutedClones(next);
    run(putMutedClones(next));
  };

  // OS notification whenever a clone transitions out of `working` (idle/offline) while
  // it isn't the selected one — driven by the server's `unread` edge. Clicking it selects
  // that clone, which closes an open ticket the same way a card click does. A muted clone
  // (or one under a muted parent) raises nothing.
  useCloneNotifications(state.hosts, mutedClones, (id) => {
    const clone = clonesById.get(id);
    if (clone) selectClone(clone);
  });

  // Linear's own answer, asked for by this browser with the presets' keys. It is the one
  // piece of the board that does not come from the control server.
  //
  // The clones' own tickets are asked for by identifier alongside the open ones. A clone
  // outlives its ticket being closed, and a closed ticket is in nobody's open queue, so
  // without this a finished clone would draw the title it was made with forever.
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

  // Which workspaces those same keys belong to, for the ticket column's title menu. Asked once
  // rather than polled: a workspace's name and slug outlast every ticket in it.
  const workspaces = useWorkspaces(presets);

  // Write a title or a body back to Linear, and put the new value on screen twice: once
  // straight away so the panel does not snap back to what it said before the edit, and again
  // from Linear's own answer a round trip later. Without the refetch the poll interval owns
  // the panel, and reopening it inside that window shows the old text.
  //
  // A refused mutation puts the old value back. The banner alone is not enough: the poll is
  // 60 seconds, so the list would otherwise sit there showing an edit Linear does not have
  // for up to a minute, with an error next to it. The rollback is on the mutation and not on
  // the refetch, because a refetch that fails after a write that landed is still a good edit.
  const editTicket = (ticket: LinearTicket, patch: { title?: string; description?: string }) => {
    upsertTicket({ ...ticket, ...patch });
    run(
      issueUpdate(keysForTeam(presets, ticket.team ?? ""), ticket, patch)
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };

  // Set a ticket Cancelled or Backlog in Linear, and take its card off the board now rather
  // than at the end of the poll. Both states are outside what the column draws, so writing the
  // new state into the list is what removes it: no separate "dropped" set to keep in step.
  //
  // A refused write puts the old state back, the same rollback `editTicket` does and for the
  // same reason: sixty seconds of a card that is not there is worse than the error alone.
  const moveTicket = (ticket: LinearTicket, next: TicketState) => {
    if (!queued(next)) advancePast(ticket);
    upsertTicket({ ...ticket, state: next });
    run(
      issueSetState(keysForTeam(presets, ticket.team ?? ""), ticket, next)
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };

  // Move a ticket into a state the operator picked by name off its team's own workflow. Both
  // halves of the optimistic write matter here: the kind is what redraws the ring, and the id
  // and name are what keep the menu's tick and the glyph's tooltip on the state just chosen.
  //
  // Not `moveTicket`. That one names a state *type* and lands on the team's first state of it,
  // which is the right answer for the column's Cancel item and the wrong one for a menu that
  // just showed the operator two states of one kind and let them pick the second.
  const setTicketState = (ticket: LinearTicket, state: TicketWorkflowState) => {
    if (!queued(state.type)) advancePast(ticket);
    upsertTicket({ ...ticket, state: state.type, stateId: state.id, stateName: state.name });
    run(
      issueSetStateId(keysForTeam(presets, ticket.team ?? ""), ticket, state.id)
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };

  // Put one label on a ticket or take it off, on screen now and in Linear a round trip later.
  // Same optimistic shape as the two writes above, and the same rollback: the pill has already
  // gone by the time the refusal lands, so the error alone would leave the panel lying for a
  // whole poll interval.
  //
  // The list is rebuilt rather than patched in place, which is what keeps the panel's own "+"
  // menu right: it offers what the ticket does not carry, and it reads that off this list.
  const setTicketLabel = (ticket: LinearTicket, label: TicketLabel, on: boolean) => {
    const labels = on
      ? [...ticket.labels, label]
      : ticket.labels.filter((l) => l.id !== label.id);
    upsertTicket({ ...ticket, labels });
    run(
      issueSetLabel(keysForTeam(presets, ticket.team ?? ""), ticket, label.id, on)
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };

  const visibleTickets = orderTickets(
    openTickets(linearTickets, state.hosts),
    ticketOrder,
  );
  // Pin a ticket's position the moment the column first draws it, rather than the first time
  // somebody drags it. Until this ran, the stored order held only the dragged ones and every
  // other card fell through to the order Linear sent, which is `orderBy: updatedAt` — so
  // editing any ticket over there re-sorted the column here.
  //
  // Converges in one pass: the write lands in the same local state this reads, so the next
  // render finds nothing left to adopt and the effect stops. `visibleTickets` is a fresh array
  // every render, which is why the guard is the returned null rather than the dependency list.
  useEffect(() => {
    const adopted = adoptTickets(visibleTickets, ticketOrder);
    if (adopted) applyTicketOrder(adopted);
  });
  // Each clone's ticket as Linear has it now, which is what the cards draw their titles and
  // their "Open in Linear" from. Built off the whole list rather than `visibleTickets`: that
  // one has every cloned ticket filtered out of it by definition.
  const liveCloneTickets = cloneTickets(state.hosts, linearTickets);
  // The selected clone's own ticket, when it has one. Its notes card holds this instead of the
  // notes editor, so a clone made from a ticket shows the ticket.
  const selectedTicket = selectedClone?.linearTicket
    ? findTicket(selectedClone.linearTicket, linearTickets)
    : null;
  // Derived: a ticket that leaves the list (cloned, closed, moved in Linear) closes its
  // panel on its own, with no cleanup to run.
  const openTicket = selection.ticket ? findTicket(selection.ticket, visibleTickets) : null;

  /** Move the panel off a ticket that is about to leave the column, onto the one below it.
   *
   *  Marking the open ticket Done is the case this is for. Its card goes, and without this the
   *  panel would go with it and leave the side column empty over a queue that still has work
   *  in it. Called by the two writes above, before their own optimistic update, so the ticket
   *  it is handing over from is still in `visibleTickets` and still has a neighbour.
   *
   *  A refused write puts the card back but leaves the panel on the new ticket. The operator
   *  moved on when they marked it, and the banner says the state did not stick. */
  const advancePast = (ticket: LinearTicket) => {
    if (!openTicket || openTicket.id !== ticket.id) return;
    const next = ticketAfter(visibleTickets, ticket.id);
    select({ ...selection, ticket: next?.id ?? null }, true);
  };
  // What each open panel's two menus offer: its team's labels and its workflow. Two lookups
  // because two panels can be up at once on two different teams; one team asked for twice
  // costs one request, the answers being cached for the session.
  const openTicketMeta = useTeamMeta(presets, openTicket);
  const cloneTicketMeta = useTeamMeta(presets, selectedTicket);
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

  // The selected clone, when it can actually parent a sub clone: managed, running, and
  // top-level (sub clones are one level deep, and the server rejects a sub clone as a parent).
  // An archived parent is refused too: provisioning a sub clone execs into the parent.
  const subCloneParent =
    selectedClone?.managed && !selectedClone.parent && !selectedClone.archived
      ? selectedClone
      : null;

  // Clones per column, for the settings editor's counts (unfiled ones ride the first).
  const columnCounts = Object.fromEntries(
    resolveColumns(columns, state.hosts).map((c) => [c.id, c.cloneIds.length]),
  );

  /** Where a parent or sub-issue row goes when somebody already made a clone for it.
   *
   *  Once work has a clone, the clone is the thing you wanted, and the ticket is no longer in
   *  the column to select anyway. Null means nobody has, and the row opens Linear. */
  const resolveToClone = (id: string) => {
    const clone = cloneForTicket(id, state.hosts);
    if (!clone) return null;
    return {
      title: `Show the clone for ${id}: ${clone.id}`,
      open: () => selectClone(clone),
    };
  };

  /** The selected clone's Linear ticket, filling the side panel's top card in place of its
   *  notes. Null when the clone has none, or when the one it names has not arrived.
   *
   *  Replacing rather than joining is the point. A clone made from a ticket is the work that
   *  ticket describes, so the ticket is what belongs over the chat. Its notes file stays on
   *  disk and `/api/notes/:id` still serves it; nothing in the UI opens it. */
  const cloneTicketCard = () => {
    if (!selectedClone?.linearTicket || !selectedTicket) return null;
    return (
      <TicketPanel
        ticket={selectedTicket}
        onCopyBranchName={copyText}
        description={
          <ClientOnly>
            <Suspense fallback={<p className="px-1 text-xs text-slate-400">Loading…</p>}>
              <TicketDescription
                key={selectedTicket.id}
                markdown={selectedTicket.description ?? ""}
                onSave={(markdown) => editTicket(selectedTicket, { description: markdown })}
              />
            </Suspense>
          </ClientOnly>
        }
        onTitleChange={(title) => editTicket(selectedTicket, { title })}
        onStateChange={(next) => setTicketState(selectedTicket, next)}
        stateOptions={cloneTicketMeta.states}
        statesLoading={cloneTicketMeta.loading}
        labelOptions={cloneTicketMeta.labels}
        labelsLoading={cloneTicketMeta.loading}
        onAddLabel={(label) => setTicketLabel(selectedTicket, label, true)}
        onRemoveLabel={(label) => setTicketLabel(selectedTicket, label, false)}
        // No "Create a clone" button: this panel is only ever drawn for a ticket that
        // already has one, and it is the clone you are looking at.
        resolveLink={resolveToClone}
      />
    );
  };

  /** What the top card holds when the ticket above did not fill it: the clone's notes, plus a
   *  line about the ticket when there was one to wait for.
   *
   *  Before the first answer the card names the ticket it is waiting on, rather than showing
   *  notes it is about to replace. After that answer a ticket that still did not resolve is one
   *  Linear will not give us: the key is gone, the issue moved workspace, or Linear is down. The
   *  card falls back to notes and says why, because a clone whose ticket cannot load is still a
   *  clone somebody needs to work in. A later poll that does resolve it takes the card back. */
  const notesCard = () => {
    if (!selectedClone) return null;
    const notes = (
      <ClientOnly>
        <Suspense
          fallback={
            <div className="p-6 text-sm text-slate-400 dark:text-slate-500">Loading editor…</div>
          }
        >
          <NotesEditorContainer key={selectedClone.id} cloneId={selectedClone.id} />
        </Suspense>
      </ClientOnly>
    );
    if (!selectedClone.linearTicket || selectedTicket) return notes;
    if (ticketsLoading) {
      return (
        <p className="px-4 text-sm text-slate-400 dark:text-slate-500">
          Loading {selectedClone.linearTicket}…
        </p>
      );
    }
    return (
      <>
        <p className="px-4 pb-2 text-xs text-slate-400 dark:text-slate-500">
          {selectedClone.linearTicket} did not load{ticketsError ? `: ${ticketsError}` : ""}.
          Showing notes.
        </p>
        {notes}
      </>
    );
  };

  return (
    <>
      <AppShellV2
        selectedClone={selectedClone}
        ticket={
          openTicket ? (
            <TicketPanel
              ticket={openTicket}
              onCopyBranchName={copyText}
              description={
                <ClientOnly>
                  <Suspense fallback={<p className="px-1 text-xs text-slate-400">Loading…</p>}>
                    <TicketDescription
                      key={openTicket.id}
                      markdown={openTicket.description ?? ""}
                      onSave={(markdown) => editTicket(openTicket, { description: markdown })}
                    />
                  </Suspense>
                </ClientOnly>
              }
              onTitleChange={(title) => editTicket(openTicket, { title })}
              // Marking it Done or Cancelled here takes the card out of the column, which
              // takes this panel with it: `openTicket` is derived from the column's own list.
              onStateChange={(next) => setTicketState(openTicket, next)}
              stateOptions={openTicketMeta.states}
              statesLoading={openTicketMeta.loading}
              labelOptions={openTicketMeta.labels}
              labelsLoading={openTicketMeta.loading}
              onAddLabel={(label) => setTicketLabel(openTicket, label, true)}
              onRemoveLabel={(label) => setTicketLabel(openTicket, label, false)}
              onCreateClone={() => {
                setTicketPrefill(openTicket.url);
                setCloneOpen(true);
              }}
              // A parent or sub-issue the board already holds is one click away, so the row
              // goes there instead of to Linear. The ticket column first, then the clones.
              resolveLink={(id) => {
                const ticket = findTicket(id, visibleTickets);
                if (ticket) {
                  return {
                    title: `Show ${ticket.id} in this panel`,
                    open: () => select({ ...selection, ticket: ticket.id }),
                  };
                }
                return resolveToClone(id);
              }}
            />
          ) : undefined
        }
        error={error}
        sideFocus={sideFocus}
        onSideFocusChange={setSideFocus}
        initialSideWidth={sideWidth}
        onSideWidthCommit={rememberSideWidth}
        // The operator's locale, read here rather than in the usage bars, so the reset-time
        // tooltips are a function of the shell's props like everything else it draws.
        locale={browserLocale()}
        rail={{
          accounts,
          accountOrder: acctOrder,
          cloneGroups,
          codexGroups,
          now,
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
          cloneTickets: liveCloneTickets,
          stats,
          cloneTokens: state.cloneTokens,
          forwards,
          operations: state.operations,
          // A ticket takes the highlight while it is open, and the clone stays activated
          // underneath, so the viewer keeps its stream. An archived clone takes the highlight
          // the same way, off the same rule: the highlight follows the panels, never the
          // viewer.
          selectedId: openTicket ? null : focusedId,
          // The card copies an `ssh -J` one-liner, and the jump target is this page's own
          // address whenever no public host is configured. That read is the browser's, so it
          // happens here and the card is handed the answer.
          sshPublicHost: sshPublicHost || window.location.hostname,
          bastionPort,
          // Picking a clone always closes the ticket, the currently selected clone
          // included: that click means "back to this clone", and there is nothing else it
          // could mean once its own card is already the one activated.
          //
          // An archived one opens its panels without activating anything. The server refuses
          // to activate it anyway, and asking would only cost a round trip to be told the
          // selection did not move.
          onSelectClone: selectClone,
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
          onCopySshCommand: copyText,
          onOpenInLinear: openInLinear,
          mutedClones,
          onToggleMuteClone: (clone) => applyMuted(toggleMuted(mutedClones, clone.id)),
          tickets: {
            tickets: visibleTickets,
            // Nothing asked yet is not an empty queue. Without this the column claims every
            // open ticket already has a clone for as long as the first fetch takes.
            loading: ticketsLoading,
            error: ticketsError,
            selectedId: openTicket?.id ?? null,
            onSelectTicket: (ticket) => select({ ...selection, ticket: ticket.id }),
            onNewTicket: () => setNewTicketOpen(true),
            workspaces,
            onOpenWorkspace: (workspace) => openInLinear(workspaceHomeUrl(workspace)),
            onOpenInLinear: openInLinear,
            onCopyBranchName: copyText,
            onCopyTicketLink: copyText,
            onCancel: (ticket) => moveTicket(ticket, "canceled"),
            onMoveToBacklog: (ticket) => moveTicket(ticket, "backlog"),
          },
          onNewCloneFromTicket: (ticket, columnId) => {
            setNewCloneColumn(columnId);
            setTicketPrefill(ticket.url);
            setCloneOpen(true);
          },
          onReorderTickets: applyTicketOrder,
          onNewClone: (columnId) => {
            setNewCloneColumn(columnId);
            setCloneOpen(true);
          },
          onMoveCard: (cloneId, toColumnId, toIndex) =>
            applyColumns(moveCard(columns, cloneId, toColumnId, toIndex)),
          onRenameColumn: (columnId, title) =>
            applyColumns(columns.map((c) => (c.id === columnId ? { ...c, title } : c))),
        }}
        notes={notesCard()}
        cloneTicket={cloneTicketCard()}
        chat={
          selectedClone ? (
            <ClientOnly>
              <Suspense
                fallback={
                  <div className="p-4 text-sm text-slate-400 dark:text-slate-500">Loading chat…</div>
                }
              >
                <ChatContainer
                  key={selectedClone.id}
                  cloneId={selectedClone.id}
                  archived={selectedClone.archived === true}
                />
              </Suspense>
            </ClientOnly>
          ) : null
        }
      />

      {newTicketOpen ? (
        <TicketModalContainer
          presets={presets}
          onClose={() => setNewTicketOpen(false)}
          // Linear answered with the whole ticket, so the column can draw it now. Opening
          // its panel is the useful next step: a ticket worth creating is one you are about
          // to work on, and the panel needs the ticket to be in the list to find it.
          onCreated={(created) => {
            upsertTicket(created);
            select({ ...selection, ticket: created.id });
            refetchTickets();
          }}
        />
      ) : null}

      {cloneOpen ? (
        <CloneModalContainer
          images={images}
          imagesLoading={imagesLoading}
          operations={state.operations}
          parentCandidate={subCloneParent}
          accounts={accounts}
          initialTicket={ticketPrefill}
          onClose={() => {
            setCloneOpen(false);
            setNewCloneColumn(null);
            setTicketPrefill("");
            // Land on the clone that was just made. The dialog only closes once its
            // operation has settled, so by the time this runs the clone either exists or
            // the create failed — hence the check, so a failed create leaves the current
            // selection alone rather than pointing at a clone that never appeared.
            const made = newClone ? clonesById.get(newClone) : null;
            if (made) selectClone(made);
            setNewClone(null);
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
              // The op's target is the new clone's id; `onClose` selects it once the
              // dialog settles, so making a clone leaves the operator looking at it.
              setNewClone(op.target);
              return op;
            })
          }
        />
      ) : null}

      {settingsOpen ? (
        <SettingsPanelContainer
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
              { id: newColumnId(title, columns), title, cloneIds: [], archive: false },
            ])
          }
          onRenameBoardColumn={(columnId, title) =>
            applyColumns(columns.map((c) => (c.id === columnId ? { ...c, title } : c)))
          }
          onSetBoardColumnArchive={(columnId, archive) =>
            applyColumns(columns.map((c) => (c.id === columnId ? { ...c, archive } : c)))
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
        <ImportAccountModalContainer
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
        <ChangeAccountModalContainer
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
  );
}
