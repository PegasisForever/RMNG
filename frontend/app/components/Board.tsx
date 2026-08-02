// The clone board: the control rail followed by the operator's columns. Cards are clones and
// columns are whatever the operator makes them; none is special-cased here.
//
// Column membership is a prop, so the board is controlled: a drop reports where the card
// landed and the container persists it. The one piece of state kept here is the in-flight
// drag arrangement, which has to update on every hover to make cards part around the
// pointer; it is dropped the moment the drag ends.
//
// Archiving rides the same gesture, driven by the target column's `archive` flag: dropping
// into a flagged column archives, dragging out of one starts the clone again. The card is
// stored in the column it was dropped in either way, so it sits still while the call
// round-trips instead of bouncing back to where it came from and forward again.
//
// Sub clones are the one thing a column does not hold. They hang under their parent's card,
// collapsed until asked for, and carry no sortable id of their own — so a drag can never
// separate a helper from the clone that spawned it, and moving the parent takes them along.
import {
  closestCorners,
  DndContext,
  DragOverlay,
  KeyboardSensor,
  pointerWithin,
  PointerSensor,
  rectIntersection,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragOverEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import { arrayMove, sortableKeyboardCoordinates } from "@dnd-kit/sortable";
import { type ReactNode, useRef, useState } from "react";

import { BoardCard, BoardCardBody, CardFrame } from "~/components/BoardCard";
import { BoardColumnPanel } from "~/components/BoardColumnPanel";
import { SidebarClone } from "~/components/SidebarClone";
import { TicketCardBody, TicketColumn, type TicketColumnProps } from "~/components/TicketColumn";
import { archivesOnDrop, resolveColumns, subCloneTree, type BoardColumn } from "~/lib/board";
import {
  ticketIdFromDrag,
  TICKET_COLUMN_ID,
  type CloneTicket,
  type LinearTicket,
} from "~/lib/tickets";
import type { Clone, Operation } from "~/lib/types";
import type { CloneTokens } from "~/lib/wire/CloneTokens";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";

/** A column as drawn. Ids are column ids. */
type Lane = { id: string; cloneIds: string[] };

export interface BoardProps {
  /** The operator's columns, in display order. */
  columns: BoardColumn[];
  /** Every clone, archived ones included. */
  clones: Clone[];
  /** Each clone's Linear ticket as Linear has it now, by clone id. A clone missing from it
   *  draws the title and link it stored when it was made. */
  cloneTickets?: Record<string, CloneTicket>;
  /** Live per-clone CPU/RAM map (the volatile `stats` SSE event). */
  stats: Record<string, ContainerStats>;
  /** All-time per-clone token totals from `ControlState.cloneTokens`. */
  cloneTokens?: Record<string, CloneTokens>;
  /** Live per-clone forward-runtime map (the `forwards` SSE event). */
  forwards?: Record<string, ForwardRuntime[]>;
  /** All operations; a card with a running one is frozen in place. */
  operations: Operation[];
  /** The clone drawn as selected, which is not always the clone the fleet is pointed at.
   *  A selected ticket owns the highlight while it is open, so the caller passes null then:
   *  exactly one thing on the board is ever highlighted, and it is whichever the operator
   *  picked last. The clone keeps the video stream and its notes underneath regardless. */
  selectedId: string | null;
  /** The `-J` jump target for each card's copied SSH command, already resolved by the
   *  container: `ssh.publicHost` from config, or this page's own address without one. */
  sshPublicHost: string;
  /** `listen.bastion` — the bastion `sshd` port those commands jump through. */
  bastionPort: number;
  /** The control rail, drawn first in the strip (see BoardRail). */
  rail: ReactNode;
  /** Unclaimed Linear tickets, drawn as a column between the rail and the operator's own.
   *  Absent ⇒ no ticket column, which is what a fleet with no Linear key gets. */
  tickets?: TicketColumnProps;
  /** A ticket was dropped into a clone column. The container opens the clone dialog with
   *  the ticket filled in; nothing about the board changes until that clone actually
   *  exists. */
  onNewCloneFromTicket?: (ticket: LinearTicket, columnId: string) => void;
  /** The ticket column was rearranged. Ids top to bottom, for the container to persist. */
  onReorderTickets?: (ticketIds: string[]) => void;
  /** Leave room at the right end of the strip for a panel floating over the board, so the
   *  last column can still be scrolled out from under it. The gutter is the panel's own
   *  width expression, `max(var(--side-panel-w), 20rem)`: the custom property is what the
   *  operator drags, and the 20rem floor is what keeps the two in step on a narrow laptop
   *  where the percentage alone would fall short of the panel's minimum. */
  gutterRight?: boolean;

  onSelectClone: (clone: Clone) => void;
  onDeleteClone: (clone: Clone) => void;
  onCommitClone: (clone: Clone) => void;
  onChangeAccountClone: (clone: Clone) => void;
  onPortForwardClone: (clone: Clone) => void;
  /** A card was dropped into a column flagged `archive` and is not archived yet. */
  onArchiveClone: (clone: Clone) => void;
  /** A card was dragged out of an `archive` column and is still archived. */
  onUnarchiveClone: (clone: Clone) => void;
  /** A card menu asked for its clone's `ssh -J …` one-liner to go on the clipboard, and for
   *  the answer to whether it landed. The ticket column asks for the same thing through
   *  `tickets`, so one container handler serves both. */
  onCopySshCommand: (command: string) => Promise<boolean>;
  /** A card menu asked to show its clone's Linear ticket, which leaves the app. The ticket
   *  column's menu asks the same way. */
  onOpenInLinear: (url: string) => void;

  /** Create a clone and file it in this column. Every column offers this, archive columns
   *  included: the flag is a rule about the drop gesture, not a lock on the column. */
  onNewClone: (columnId: string) => void;
  /** Where a card ended up. */
  onMoveCard: (cloneId: string, toColumnId: string, toIndex: number) => void;
  /** Rename from the board itself (double-click a column title). Adding, deleting and
   *  reordering columns live in the settings panel. */
  onRenameColumn: (columnId: string, title: string) => void;
}

/** What the card is being dropped on.
 *
 *  A distance-based strategy alone cannot hit an empty column: the column body is a large
 *  rectangle whose corners sit far from the dragged card, so a small card in the next
 *  column over wins even while the pointer is inside the empty one. Asking what the pointer
 *  is actually inside settles it, and it answers with the card AND its column when both
 *  apply, so ordinary between-cards drops still land at the right index.
 *
 *  The fallbacks cover the keyboard sensor, which has no pointer at all. */
const collisionDetection: CollisionDetection = (args) => {
  const pointer = pointerWithin(args);
  if (pointer.length > 0) return pointer;
  const intersecting = rectIntersection(args);
  return intersecting.length > 0 ? intersecting : closestCorners(args);
};

/** The lane holding `cardId`, or the lane whose own id it is (dropping on a column's empty
 *  body reports the column). */
function laneOf(lanes: Lane[], id: string): Lane | undefined {
  return lanes.find((lane) => lane.id === id) ?? lanes.find((lane) => lane.cloneIds.includes(id));
}

/** The arrangement with `activeId` moved to wherever `overId` currently sits. Pure, so the
 *  same call resolves a hover preview and the final drop. */
export function arrange(lanes: Lane[], activeId: string, overId: string): Lane[] {
  const from = lanes.find((lane) => lane.cloneIds.includes(activeId));
  const to = laneOf(lanes, overId);
  if (!from || !to) return lanes;

  const overIndex = to.cloneIds.indexOf(overId);
  if (from.id === to.id) {
    const fromIndex = from.cloneIds.indexOf(activeId);
    if (overIndex < 0 || overIndex === fromIndex) return lanes;
    return lanes.map((lane) =>
      lane.id === from.id
        ? { ...lane, cloneIds: arrayMove(lane.cloneIds, fromIndex, overIndex) }
        : lane,
    );
  }
  // Over the column itself rather than one of its cards ⇒ the end of the list.
  const insertAt = overIndex < 0 ? to.cloneIds.length : overIndex;
  return lanes.map((lane) => {
    if (lane.id === from.id) {
      return { ...lane, cloneIds: lane.cloneIds.filter((id) => id !== activeId) };
    }
    if (lane.id === to.id) {
      const cloneIds = [...lane.cloneIds];
      cloneIds.splice(insertAt, 0, activeId);
      return { ...lane, cloneIds };
    }
    return lane;
  });
}

/** `next`, or `prev` itself when the two arrangements hold the same ids in the same order.
 *
 *  Identity is load-bearing. dnd-kit re-measures every droppable whenever this state changes,
 *  and each measurement fires `onDragOver` again — so an arrangement that allocates a fresh
 *  array while describing the same layout keeps that cycle alive until React gives up with
 *  "Maximum update depth exceeded". `arrange` already returns its input untouched when a move
 *  is a no-op within one lane; this covers the cross-lane branch, which always allocates. */
/** How far the pointer must travel, in px, before the board will rearrange again.
 *
 *  Under the `PointerSensor`'s own 5px activation distance, so it costs nothing a drag has
 *  not already paid for, and small enough that reordering still feels immediate. */
const REARRANGE_MIN_TRAVEL = 4;

function settle(prev: Lane[], next: Lane[]): Lane[] {
  if (prev === next) return prev;
  if (prev.length !== next.length) return next;
  const unchanged = prev.every((lane, i) => {
    const other = next[i];
    return (
      lane.id === other.id &&
      lane.cloneIds.length === other.cloneIds.length &&
      lane.cloneIds.every((id, j) => id === other.cloneIds[j])
    );
  });
  return unchanged ? prev : next;
}

export function Board({
  columns,
  clones,
  cloneTickets = {},
  stats,
  cloneTokens = {},
  forwards = {},
  operations,
  selectedId,
  sshPublicHost,
  bastionPort,
  rail,
  tickets,
  onNewCloneFromTicket,
  onReorderTickets,
  gutterRight = false,
  onSelectClone,
  onDeleteClone,
  onCommitClone,
  onChangeAccountClone,
  onPortForwardClone,
  onArchiveClone,
  onUnarchiveClone,
  onCopySshCommand,
  onOpenInLinear,
  onNewClone,
  onMoveCard,
  onRenameColumn,
}: BoardProps) {
  const [dragId, setDragId] = useState<string | null>(null);
  const [preview, setPreview] = useState<Lane[] | null>(null);
  // Where the pointer was when this drag last rearranged. A ref, not state: it gates an
  // event handler and must never itself cause a render.
  const arrangedAt = useRef<{ x: number; y: number } | null>(null);
  // Which parents have their sub clones showing. Collapsed by default: a parent working
  // through a task spawns helpers constantly, and every one of them expanded would bury the
  // clones the operator actually filed.
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const toggleExpand = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  // The ticket column's in-flight arrangement, the same idea as `preview` for the lanes:
  // held only while a ticket is in the air, dropped the moment the drag ends.
  const [ticketPreview, setTicketPreview] = useState<string[] | null>(null);

  const { childrenByParent } = subCloneTree(clones);
  const byId = new Map(clones.map((clone) => [clone.id, clone]));
  const titles = new Map(columns.map((column) => [column.id, column.title]));
  const base: Lane[] = resolveColumns(columns, clones).map((column) => ({
    id: column.id,
    cloneIds: column.cloneIds,
  }));
  const lanes = preview ?? base;

  // The ticket column, arranged by the in-flight preview when there is one. The container
  // owns the persisted order, so `tickets.tickets` already arrives in it.
  const ticketIds = tickets?.tickets.map((t) => t.id) ?? [];
  const ticketById = new Map((tickets?.tickets ?? []).map((t) => [t.id, t]));
  const ticketOrder = ticketPreview ?? ticketIds;
  const orderedTickets = tickets
    ? ticketOrder.flatMap((id) => {
        const ticket = ticketById.get(id);
        return ticket ? [ticket] : [];
      })
    : [];

  const sensors = useSensors(
    // The same 5px activation distance the sidebar used, so a plain click still selects
    // the card instead of starting a drag.
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const opFor = (id: string) =>
    operations.find((op) => op.target === id && op.status === "running");
  // One clone provisions at a time, so every column's button waits out the one in flight.
  const cloning = operations.some((op) => op.kind === "clone" && op.status === "running");

  const cardProps = (clone: Clone) => ({
    clone,
    ticket: cloneTickets[clone.id],
    stats: stats[clone.id],
    tokens: cloneTokens[clone.id],
    forwardRuntime: forwards[clone.id],
    op: opFor(clone.id),
    selected: selectedId === clone.id,
    sshPublicHost,
    bastionPort,
    onSelect: () => onSelectClone(clone),
    onDelete: () => onDeleteClone(clone),
    onCommit: () => onCommitClone(clone),
    onChangeAccount: () => onChangeAccountClone(clone),
    onPortForward: () => onPortForwardClone(clone),
    onArchive: () => onArchiveClone(clone),
    onUnarchive: () => onUnarchiveClone(clone),
    onCopySshCommand,
    onOpenInLinear,
  });

  /** A clone's sub-clone rows, or null when they are collapsed or it has none.
   *
   *  Sub clones share their parent's card rather than taking one each, and carry no sortable
   *  id: a drag cannot separate a helper from the clone that spawned it, and moving the
   *  parent takes them along. The hairline is the only thing dividing the rows inside one
   *  frame.
   *
   *  The column and the drag overlay both draw through this, so a card in flight carries the
   *  same rows as the one it lifted out of. */
  const subRows = (parentId: string) =>
    expanded.has(parentId)
      ? (childrenByParent.get(parentId) ?? []).map((child) => (
          <div key={child.id} className="border-t border-slate-900/10 dark:border-white/10">
            <SidebarClone isChild {...cardProps(child)} />
          </div>
        ))
      : null;

  const onDragStart = (event: DragStartEvent) => {
    arrangedAt.current = null;
    setDragId(String(event.active.id));
  };

  const onDragOver = (event: DragOverEvent) => {
    if (!event.over) return;
    const activeId = String(event.active.id);
    const overId = String(event.over.id);

    // A ticket is not in any lane, so the clone columns never rearrange around one. Inside
    // its own column it sorts like any card; over a clone column it stops moving, because
    // the drop there makes a clone rather than filing the ticket.
    const activeTicket = ticketIdFromDrag(activeId);
    if (activeTicket) {
      const overTicket = ticketIdFromDrag(overId);
      if (!overTicket && overId !== TICKET_COLUMN_ID) return;
      setTicketPreview((prev) => {
        const current = prev ?? ticketIds;
        const from = current.indexOf(activeTicket);
        // Over the well itself rather than a card ⇒ the end of the list.
        const to = overTicket ? current.indexOf(overTicket) : current.length - 1;
        return from < 0 || to < 0 || from === to ? current : arrayMove(current, from, to);
      });
      return;
    }

    // Rearranging moves the dragged card out from under the pointer, which drops the pointer
    // onto a different card, which rearranges again. With a stationary pointer that is a
    // two-state flip that runs until React throws "Maximum update depth exceeded". Requiring
    // the pointer to have actually travelled breaks it: a layout shift alone can never
    // justify the next move, and only the operator can.
    const { x, y } = event.delta;
    const since = arrangedAt.current;
    if (since && Math.hypot(x - since.x, y - since.y) < REARRANGE_MIN_TRAVEL) return;

    const from = preview ?? base;
    const next = settle(from, arrange(from, activeId, overId));
    if (next === from) return;
    arrangedAt.current = { x, y };
    setPreview(next);
  };

  const onDragEnd = (event: DragEndEvent) => {
    const activeId = String(event.active.id);

    const ticketId = ticketIdFromDrag(activeId);
    if (ticketId) {
      const settledOrder = ticketPreview ?? ticketIds;
      setPreview(null);
      setTicketPreview(null);
      setDragId(null);

      const overId = event.over ? String(event.over.id) : null;
      // Still in its own column ⇒ this was a reorder. Report it only when it changed
      // something, so a click that drifted five pixels does not write an order.
      if (overId && (ticketIdFromDrag(overId) || overId === TICKET_COLUMN_ID)) {
        if (settledOrder.join("\u0000") !== ticketIds.join("\u0000")) {
          onReorderTickets?.(settledOrder);
        }
        return;
      }

      const column = overId ? laneOf(lanes, overId) : undefined;
      const ticket = ticketById.get(ticketId);
      // Dropped on nothing, or on the rail: no column means no clone, and the card simply
      // returns to the ticket list.
      if (column && ticket) onNewCloneFromTicket?.(ticket, column.id);
      return;
    }

    const settled = event.over
      ? arrange(preview ?? base, activeId, String(event.over.id))
      : (preview ?? base);
    arrangedAt.current = null;
    setPreview(null);
    setDragId(null);

    const landed = settled.find((lane) => lane.cloneIds.includes(activeId));
    const clone = byId.get(activeId);
    if (!landed || !clone) return;
    const toIndex = landed.cloneIds.indexOf(activeId);

    const started = base.find((lane) => lane.cloneIds.includes(activeId));
    if (started?.id === landed.id && started.cloneIds.indexOf(activeId) === toIndex) return;

    // The column decides; the clone's own flag says whether the server has caught up yet.
    const wasArchived = clone.archived === true;
    const nowArchived = archivesOnDrop(columns, landed.id);
    if (nowArchived && !wasArchived) onArchiveClone(clone);
    if (!nowArchived && wasArchived) onUnarchiveClone(clone);
    onMoveCard(activeId, landed.id, toIndex);
  };

  const dragged = dragId ? byId.get(dragId) : undefined;
  const draggedTicketId = dragId ? ticketIdFromDrag(dragId) : null;
  const draggedTicket = draggedTicketId ? ticketById.get(draggedTicketId) : undefined;

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={collisionDetection}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
      onDragCancel={() => {
        arrangedAt.current = null;
        setPreview(null);
        setTicketPreview(null);
        setDragId(null);
      }}
    >
      <div
        className={`flex h-full min-h-0 items-stretch gap-3 overflow-x-auto bg-slate-50 p-3 dark:bg-slate-950 ${
          gutterRight ? "pr-[max(var(--side-panel-w,30%),20rem)]" : ""
        }`}
      >
        {rail}

        {tickets ? (
          <TicketColumn
            {...tickets}
            tickets={orderedTickets}
            // The menu names no column, so a clone made that way is filed in the first one,
            // which is where an unfiled clone lands anyway.
            onCreateClone={
              onNewCloneFromTicket && lanes[0]
                ? (ticket) => onNewCloneFromTicket(ticket, lanes[0].id)
                : undefined
            }
          />
        ) : null}

        {lanes.map((lane) => (
          <BoardColumnPanel
            key={lane.id}
            id={lane.id}
            title={titles.get(lane.id) ?? lane.id}
            cloneIds={lane.cloneIds}
            empty="Drop a clone here."
            onRename={(title) => onRenameColumn(lane.id, title)}
            onNewClone={() => onNewClone(lane.id)}
            newCloneBusy={cloning}
          >
            {lane.cloneIds.map((id) => {
              const clone = byId.get(id);
              if (!clone) return null;
              const children = childrenByParent.get(id) ?? [];
              const open = expanded.has(id);
              return (
                <BoardCard
                  key={id}
                  id={id}
                  {...cardProps(clone)}
                  childCount={children.length}
                  expanded={open}
                  onToggleExpand={() => toggleExpand(id)}
                >
                  {subRows(id)}
                </BoardCard>
              );
            })}
          </BoardColumnPanel>
        ))}
      </div>

      <DragOverlay>
        {dragged ? (
          <BoardCardBody
            lifted
            {...cardProps(dragged)}
            childCount={(childrenByParent.get(dragged.id) ?? []).length}
            expanded={expanded.has(dragged.id)}
          >
            {subRows(dragged.id)}
          </BoardCardBody>
        ) : null}
        {/* No width wrapper on either card. dnd-kit sizes the overlay to the node that was
            picked up, and a `w-80` here would draw the ticket at the column's full width
            rather than the card's, which is that minus the well's padding. */}
        {draggedTicket ? (
          <CardFrame lifted>
            <TicketCardBody ticket={draggedTicket} />
          </CardFrame>
        ) : null}
      </DragOverlay>
    </DndContext>
  );
}
