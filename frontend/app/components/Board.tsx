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
import { type ReactNode, useState } from "react";

import { BoardCard, BoardCardBody } from "~/components/BoardCard";
import { BoardColumnPanel } from "~/components/BoardColumnPanel";
import { archivesOnDrop, resolveColumns, type BoardColumn } from "~/lib/board";
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
  /** Live per-clone CPU/RAM map (the volatile `stats` SSE event). */
  stats: Record<string, ContainerStats>;
  /** All-time per-clone token totals from `ControlState.cloneTokens`. */
  cloneTokens?: Record<string, CloneTokens>;
  /** Live per-clone forward-runtime map (the `forwards` SSE event). */
  forwards?: Record<string, ForwardRuntime[]>;
  /** All operations; a card with a running one is frozen in place. */
  operations: Operation[];
  selectedId: string | null;
  /** `ssh.publicHost` (config) — the `-J` jump target for each card's copied SSH command. */
  sshPublicHost: string;
  /** `listen.bastion` — the bastion `sshd` port those commands jump through. */
  bastionPort: number;
  /** The control rail, drawn first in the strip (see BoardRail). */
  rail: ReactNode;
  /** Leave room at the right end of the strip for a panel floating over the board, so the
   *  last column can still be scrolled out from under it. The gutter matches the panel's
   *  own width, `max(30%, 20rem)`, which is what keeps the two in step on a narrow laptop
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

export function Board({
  columns,
  clones,
  stats,
  cloneTokens = {},
  forwards = {},
  operations,
  selectedId,
  sshPublicHost,
  bastionPort,
  rail,
  gutterRight = false,
  onSelectClone,
  onDeleteClone,
  onCommitClone,
  onChangeAccountClone,
  onPortForwardClone,
  onArchiveClone,
  onUnarchiveClone,
  onNewClone,
  onMoveCard,
  onRenameColumn,
}: BoardProps) {
  const [dragId, setDragId] = useState<string | null>(null);
  const [preview, setPreview] = useState<Lane[] | null>(null);

  const byId = new Map(clones.map((clone) => [clone.id, clone]));
  const titles = new Map(columns.map((column) => [column.id, column.title]));
  const base: Lane[] = resolveColumns(columns, clones).map((column) => ({
    id: column.id,
    cloneIds: column.cloneIds,
  }));
  const lanes = preview ?? base;

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
  });

  const onDragStart = (event: DragStartEvent) => setDragId(String(event.active.id));

  const onDragOver = (event: DragOverEvent) => {
    if (!event.over) return;
    const activeId = String(event.active.id);
    const overId = String(event.over.id);
    setPreview((prev) => arrange(prev ?? base, activeId, overId));
  };

  const onDragEnd = (event: DragEndEvent) => {
    const activeId = String(event.active.id);
    const settled = event.over
      ? arrange(preview ?? base, activeId, String(event.over.id))
      : (preview ?? base);
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

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={collisionDetection}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
      onDragCancel={() => {
        setPreview(null);
        setDragId(null);
      }}
    >
      <div
        className={`flex h-full min-h-0 items-stretch gap-3 overflow-x-auto bg-slate-50 p-3 dark:bg-slate-950 ${
          gutterRight ? "pr-[max(30%,20rem)]" : ""
        }`}
      >
        {rail}

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
              return clone ? <BoardCard key={id} id={id} {...cardProps(clone)} /> : null;
            })}
          </BoardColumnPanel>
        ))}
      </div>

      <DragOverlay>
        {dragged ? <BoardCardBody lifted {...cardProps(dragged)} /> : null}
      </DragOverlay>
    </DndContext>
  );
}
