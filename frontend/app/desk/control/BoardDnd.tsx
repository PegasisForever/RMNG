// Drag and drop on the control panel (the arrangement is `~/lib/boardDrag.ts`'s): a clone moves within its column or to another one, and a ticket
// moves within the tickets' column or, dropped on a clone column, asks for a clone. Narrow, the
// Clones page's sections are the columns, and the Tickets page is the tickets' column.
//
// A mouse drags after 5 px, so a click still selects; a finger drags after a 250 ms press, so a
// swipe still scrolls. What follows the pointer is the views' own card (`overlays`), portalled
// to the body: the control panel's box moves (`translateY`), and fixed things in it would be off.
import {
  closestCenter,
  closestCorners,
  type CollisionDetection,
  DndContext,
  type DragEndEvent,
  type DragOverEvent,
  DragOverlay,
  type DragStartEvent,
  MouseSensor,
  pointerWithin,
  rectIntersection,
  TouchSensor,
  useDroppable,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import { arrayMove, SortableContext, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import {
  createContext,
  type CSSProperties,
  type MutableRefObject,
  type ReactNode,
  useContext,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

import { arrange, type Lane, laneOf, REARRANGE_MIN_TRAVEL, settle } from "~/lib/boardDrag";
import { TICKET_COLUMN_ID, ticketIdFromDrag } from "~/lib/tickets";

/** The card a view draws for a dragged clone or ticket. */
type Overlays = { clone?: (id: string) => ReactNode; ticket?: (id: string) => ReactNode };

type Dnd = {
  /** The columns as they are while a card is in the air, or null. */
  lanes: Lane[] | null;
  /** The tickets' order while one is in the air, or null. */
  ticketOrder: string[] | null;
  overlays: MutableRefObject<Overlays>;
};

const DndCtx = createContext<Dnd | null>(null);

/** The drag in progress, when the view is in a `BoardDnd`. */
export const useBoardDnd = () => useContext(DndCtx);

/** What the card is being dropped on: what is under the pointer; in a column but between its
 *  cards, the nearest card. A column's cards are those inside it both ways, so it holds for the
 *  narrow page's sections, which are stacked, as for the wide columns, which are side by side. */
const collisionDetection: CollisionDetection = (args) => {
  const pointer = pointerWithin(args);
  const at = args.pointerCoordinates;
  const column = pointer.length === 1 ? args.droppableRects.get(pointer[0].id) : undefined;
  if (at && column) {
    const cards = args.droppableContainers.filter((c) => {
      const r = args.droppableRects.get(c.id);
      return (
        c.data.current?.sortable &&
        r &&
        r.left >= column.left &&
        r.right <= column.right &&
        r.top >= column.top &&
        r.bottom <= column.bottom
      );
    });
    const isCard = cards.some((c) => c.id === pointer[0].id);
    const belowAll = cards.every((c) => args.droppableRects.get(c.id)!.bottom <= at.y);
    if (!isCard && !belowAll && cards.length) return closestCenter({ ...args, droppableContainers: cards });
  }
  if (pointer.length > 0) return pointer;
  const intersecting = rectIntersection(args);
  return intersecting.length > 0 ? intersecting : closestCorners(args);
};

export function BoardDnd({
  lanes,
  tickets,
  onMoveCard,
  onReorderTickets,
  onTicketToColumn,
  children,
}: {
  /** The clone columns, each with its clones' ids in order; absent: no clones here. */
  lanes?: Lane[];
  /** The tickets' ids in order; absent: no tickets here. */
  tickets?: string[];
  /** Where a clone was let go: its column, and its place in it. */
  onMoveCard?: (cloneId: string, columnId: string, index: number) => void;
  onReorderTickets?: (ticketIds: string[]) => void;
  /** A ticket was let go on a clone column: make a clone for it there. */
  onTicketToColumn?: (ticketId: string, columnId: string) => void;
  children: ReactNode;
}) {
  const [dragId, setDragId] = useState<string | null>(null);
  const [preview, setPreview] = useState<Lane[] | null>(null);
  const [ticketPreview, setTicketPreview] = useState<string[] | null>(null);
  const arrangedAt = useRef<{ x: number; y: number } | null>(null);
  const overlays = useRef<Overlays>({});
  const base = lanes ?? [];
  const ticketIds = tickets ?? [];

  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 5 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 250, tolerance: 8 } }),
  );

  const end = () => {
    arrangedAt.current = null;
    setPreview(null);
    setTicketPreview(null);
    setDragId(null);
  };

  const onDragStart = (e: DragStartEvent) => {
    arrangedAt.current = null;
    setDragId(String(e.active.id));
    navigator.vibrate?.(10);
  };

  const onDragOver = (e: DragOverEvent) => {
    if (!e.over) return;
    const activeId = String(e.active.id);
    const overId = String(e.over.id);
    const activeTicket = ticketIdFromDrag(activeId);
    if (activeTicket) {
      // Over a clone column it stays where it was: the drop there makes a clone
      const overTicket = ticketIdFromDrag(overId);
      if (!overTicket && overId !== TICKET_COLUMN_ID) return;
      setTicketPreview((prev) => {
        const current = prev ?? ticketIds;
        const from = current.indexOf(activeTicket);
        const to = overTicket ? current.indexOf(overTicket) : current.length - 1;
        return from < 0 || to < 0 || from === to ? current : arrayMove(current, from, to);
      });
      return;
    }
    // Only a pointer that moved rearranges again (see `REARRANGE_MIN_TRAVEL`)
    const { x, y } = e.delta;
    const since = arrangedAt.current;
    if (since && Math.hypot(x - since.x, y - since.y) < REARRANGE_MIN_TRAVEL) return;
    const from = preview ?? base;
    const next = settle(from, arrange(from, activeId, overId));
    if (next === from) return;
    arrangedAt.current = { x, y };
    setPreview(next);
  };

  const onDragEnd = (e: DragEndEvent) => {
    const activeId = String(e.active.id);
    const overId = e.over ? String(e.over.id) : null;
    const ticketId = ticketIdFromDrag(activeId);
    if (ticketId) {
      const order = ticketPreview ?? ticketIds;
      end();
      if (overId && (ticketIdFromDrag(overId) || overId === TICKET_COLUMN_ID)) {
        if (order.join("\u0000") !== ticketIds.join("\u0000")) onReorderTickets?.(order);
        return;
      }
      const column = overId ? laneOf(base, overId) : undefined;
      if (column) onTicketToColumn?.(ticketId, column.id);
      return;
    }
    const settled = overId ? arrange(preview ?? base, activeId, overId) : (preview ?? base);
    end();
    const landed = settled.find((l) => l.cloneIds.includes(activeId));
    if (!landed) return;
    const index = landed.cloneIds.indexOf(activeId);
    const started = base.find((l) => l.cloneIds.includes(activeId));
    if (started?.id === landed.id && started.cloneIds.indexOf(activeId) === index) return;
    onMoveCard?.(activeId, landed.id, index);
  };

  const overlay = dragId
    ? (ticketIdFromDrag(dragId) ? overlays.current.ticket?.(ticketIdFromDrag(dragId)!) : overlays.current.clone?.(dragId)) ?? null
    : null;

  return (
    <DndCtx.Provider value={{ lanes: preview, ticketOrder: ticketPreview, overlays }}>
      <DndContext
        sensors={sensors}
        collisionDetection={collisionDetection}
        onDragStart={onDragStart}
        onDragOver={onDragOver}
        onDragEnd={onDragEnd}
        onDragCancel={end}
      >
        {children}
        {typeof document === "undefined"
          ? null
          : createPortal(
              <DragOverlay>
                {overlay ? (
                  // Lifted off the page: solid, with a long shadow
                  <div className="overflow-hidden rounded-lg bg-white shadow-[0_12px_32px_rgb(0_0_0/0.3)] dark:bg-neutral-800">
                    {overlay}
                  </div>
                ) : null}
              </DragOverlay>,
              document.body,
            )}
      </DndContext>
    </DndCtx.Provider>
  );
}

/** A column that takes drops: `id` is its column's (or `TICKET_COLUMN_ID`), `items` its cards'
 *  drag ids in order. Without a `BoardDnd` round it, a plain element. */
export function DropColumn({
  id,
  items,
  as: Tag = "ul",
  className,
  children,
}: {
  id: string;
  items: string[];
  as?: "ul" | "section" | "div";
  className?: string;
  children: ReactNode;
}) {
  const dnd = useBoardDnd();
  if (!dnd) return <Tag className={className}>{children}</Tag>;
  return (
    <Droppable id={id} as={Tag} className={className}>
      <SortableContext items={items} strategy={verticalListSortingStrategy}>
        {children}
      </SortableContext>
    </Droppable>
  );
}

function Droppable({ id, as: Tag, className, children }: { id: string; as: "ul" | "section" | "div"; className?: string; children: ReactNode }) {
  const { setNodeRef } = useDroppable({ id });
  return (
    <Tag ref={setNodeRef} className={className}>
      {children}
    </Tag>
  );
}

/** A card that drags: the whole card is the handle (`children` takes its listeners, to spread
 *  where the card wants them). Without a `BoardDnd`, a plain item. */
export function DragItem({
  id,
  disabled = false,
  className,
  children,
}: {
  id: string;
  disabled?: boolean;
  className?: string;
  children: (listeners: Record<string, unknown> | undefined) => ReactNode;
}) {
  const dnd = useBoardDnd();
  if (!dnd) return <li className={className}>{children(undefined)}</li>;
  return (
    <Sortable id={id} disabled={disabled} className={className}>
      {children}
    </Sortable>
  );
}

function Sortable({
  id,
  disabled,
  className,
  children,
}: {
  id: string;
  disabled: boolean;
  className?: string;
  children: (listeners: Record<string, unknown> | undefined) => ReactNode;
}) {
  const { setNodeRef, listeners, transform, transition, isDragging } = useSortable({ id, disabled });
  // Pointer drag only: no keyboard lift on a Space or an Enter meant for a button in the card
  const { onKeyDown: _lift, ...drag } = listeners ?? {};
  const style: CSSProperties = {
    transform: CSS.Translate.toString(transform),
    transition,
    // The card keeps its place as a hole while its copy follows the pointer
    opacity: isDragging ? 0 : 1,
  };
  return (
    // No callout or selection on a long press: it is the start of a drag
    <li ref={setNodeRef} style={style} className={`[-webkit-touch-callout:none] ${className ?? ""}`}>
      {children(disabled ? undefined : drag)}
    </li>
  );
}
