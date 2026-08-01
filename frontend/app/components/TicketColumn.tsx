// The board's leftmost work column: Linear tickets in Todo or In Progress that no clone has
// been created for. Drag one into a clone column to start it.
//
// It is a column, not a rail section, because that is what makes the gesture obvious: the
// tickets sit in the same well the cards do, so dragging one right into a column reads as
// the same move as dragging a card, and the board already knows how to receive it.
//
// The order is the operator's, not Linear's: cards sort within the column, and the well
// takes drops so one dragged back lands where it was let go.
//
// The list itself is pushed, never pulled. The server holds the Linear keys and polls with
// them, so a ticket somebody moves in Linear appears or disappears here on its own, and
// there is no refresh button to press: a button that has to be pressed is a promise that
// the list is stale until you do.
import { useDroppable } from "@dnd-kit/core";
import { SortableContext, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { CircleDashed, CircleX, ExternalLink, GitBranch, Link2, Plus } from "lucide-react";
import type { CSSProperties } from "react";

import { CardFrame } from "~/components/BoardCard";
import { MenuDivider, MenuItem, OverflowMenu } from "~/components/OverflowMenu";
import {
  ColumnHeader,
  ColumnShell,
  ColumnWell,
  COLUMN_TITLE,
} from "~/components/BoardColumnPanel";
import { copyText } from "~/lib/clipboard";
import { branchNameOf, ticketDragId, TICKET_COLUMN_ID, type LinearTicket } from "~/lib/tickets";
import { workspaceBadge } from "~/lib/workspace";

export const STATE_LABEL: Record<LinearTicket["state"], string> = {
  backlog: "Backlog",
  todo: "Todo",
  in_progress: "In progress",
  done: "Done",
  canceled: "Cancelled",
};

const STATE_COLOR: Record<LinearTicket["state"], string> = {
  backlog: "text-slate-400 dark:text-slate-500",
  todo: "text-slate-400 dark:text-slate-500",
  in_progress: "text-amber-500",
  done: "text-indigo-500",
  canceled: "text-slate-400 dark:text-slate-500",
};

/** Linear's state mark, redrawn: a ring that fills as the work moves. Backlog is dashed,
 *  Todo is the plain empty ring, In progress is half filled, Done is filled with a tick, and
 *  Cancelled is struck through. The same glyphs Linear puts on its own column headers.
 *
 *  A ring rather than the clone cards' solid dot. A clone's dot reports something changing
 *  while you watch it, and this reports a state somebody set in another tool, so drawing
 *  them the same way would claim they are the same kind of fact. */
export function StateIcon({ state }: { state: LinearTicket["state"] }) {
  const label = STATE_LABEL[state];
  return (
    <svg
      viewBox="0 0 14 14"
      role="img"
      aria-label={label}
      className={`size-3.5 shrink-0 ${STATE_COLOR[state]}`}
    >
      <title>{label}</title>
      <circle
        cx="7"
        cy="7"
        r="5.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeDasharray={state === "backlog" ? "2.5 2" : undefined}
      />
      {state === "in_progress" ? (
        <path d="M7 3.5 A3.5 3.5 0 0 1 7 10.5 Z" fill="currentColor" />
      ) : null}
      {state === "done" ? (
        <>
          <circle cx="7" cy="7" r="5.5" fill="currentColor" />
          <path
            d="M4.5 7.2 L6.3 9 L9.6 5.4"
            fill="none"
            stroke="white"
            strokeWidth="1.6"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </>
      ) : null}
      {state === "canceled" ? (
        <path d="M4.6 4.6 L9.4 9.4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      ) : null}
    </svg>
  );
}

/** Linear's priority scale, 1 (urgent) to 4 (low). */
export const PRIORITY_LABEL: Record<number, string> = {
  1: "Urgent",
  2: "High",
  3: "Medium",
  4: "Low",
};

/** Linear's priority glyph, redrawn: three ascending bars filled up to the level, and a
 *  filled mark for Urgent because urgent is not the top of the scale so much as a different
 *  claim about the work.
 *
 *  It is the glyph and not a worded pill because that is what the operator already reads in
 *  Linear, and because four coloured pills down a column is four colours competing with the
 *  labels, which carry the operator's own scheme and should win. */
export function PriorityIcon({ level }: { level: number }) {
  const label = PRIORITY_LABEL[level];
  if (level === 1) {
    return (
      <svg viewBox="0 0 16 16" className="size-3.5 shrink-0" role="img" aria-label={label}>
        <title>{label}</title>
        <rect x="1" y="1" width="14" height="14" rx="3.5" className="fill-orange-500" />
        <rect x="7" y="4" width="2" height="5" rx="1" className="fill-white" />
        <rect x="7" y="10.5" width="2" height="2" rx="1" className="fill-white" />
      </svg>
    );
  }
  // 2 High fills all three bars, 3 Medium two, 4 Low one. The rest stay faint rather than
  // disappearing, so every glyph is the same shape and only its fill differs.
  const filled = level === 2 ? 3 : level === 3 ? 2 : 1;
  const bars = [
    { x: 1.5, y: 9.5, h: 5 },
    { x: 6.5, y: 6.5, h: 8 },
    { x: 11.5, y: 3.5, h: 11 },
  ];
  return (
    <svg viewBox="0 0 16 16" className="size-3.5 shrink-0" role="img" aria-label={label}>
      <title>{label}</title>
      {bars.map((bar, i) => (
        <rect
          key={bar.x}
          x={bar.x}
          y={bar.y}
          width="3"
          height={bar.h}
          rx="1"
          className={i < filled ? "fill-slate-500 dark:fill-slate-300" : "fill-slate-300 dark:fill-slate-600"}
        />
      ))}
    </svg>
  );
}

/** A Linear label: an outlined pill with the label's own colour as a dot. The colour is a
 *  `#rrggbb` string from Linear, so it rides an inline style; everything else is the same
 *  hairline the cards wear. */
export function LabelPill({ name, color }: { name: string; color: string }) {
  return (
    <span className="inline-flex min-w-0 shrink items-center gap-1 rounded-full border border-slate-300 px-1.5 py-0.5 text-[11px] leading-none text-slate-600 dark:border-slate-600 dark:text-slate-300">
      <span
        aria-hidden
        className="size-2 shrink-0 rounded-full"
        style={{ backgroundColor: color }}
      />
      <span className="truncate">{name}</span>
    </span>
  );
}

/** The team key that colours the badge. Derived from the id when Linear did not say, so the
 *  badge can never disagree with the identifier printed inside it. */
function teamOf(ticket: LinearTicket): string {
  return ticket.team ?? ticket.id.split("-")[0];
}


/** The ticket's ⋮ menu, on the same shell the clone cards use.
 *
 *  Three kinds of item, in that order: one that starts work here, shortcuts out to Linear,
 *  and two that write a new state back to it. The writes are the only things on this board
 *  that change a ticket, and both mean the same thing to this column — a ticket that is
 *  neither Todo nor In progress is not waiting to be started, so it leaves the list. */
function TicketMenu({
  ticket,
  onCreateClone,
  onCancel,
  onMoveToBacklog,
}: {
  ticket: LinearTicket;
  onCreateClone?: () => void;
  onCancel?: () => void;
  onMoveToBacklog?: () => void;
}) {
  return (
    <OverflowMenu label={`actions for ${ticket.id}`}>
      {onCreateClone ? (
        <MenuItem icon={Plus} label="Create a clone…" onClick={onCreateClone} />
      ) : null}
      <MenuItem
        icon={ExternalLink}
        label="Open in Linear"
        onClick={() => window.open(ticket.url, "_blank", "noopener,noreferrer")}
      />
      <MenuDivider />
      <MenuItem
        icon={GitBranch}
        label="Copy branch name"
        onClick={() => void copyText(branchNameOf(ticket))}
      />
      <MenuItem icon={Link2} label="Copy link" onClick={() => void copyText(ticket.url)} />
      {onMoveToBacklog || onCancel ? <MenuDivider /> : null}
      {onMoveToBacklog ? (
        <MenuItem icon={CircleDashed} label="Move to backlog" onClick={onMoveToBacklog} />
      ) : null}
      {onCancel ? (
        <MenuItem icon={CircleX} label="Cancel ticket" onClick={onCancel} danger />
      ) : null}
    </OverflowMenu>
  );
}

/** The card's own markup, without the drag wiring, so the drag overlay can render the same
 *  thing under the pointer.
 *
 *  Laid out as a clone card is: a muted metadata row up top, then the title line carrying
 *  the `WE-301` badge inline with the title, so a wrapped title flows back to the card's
 *  left edge instead of indenting under the badge. The badge takes its colour from the same
 *  `workspaceBadge` the clone cards use, so a ticket and the clone made from it are the
 *  same colour on the same board.
 *
 *  No lifecycle dot and no state word. Linear's own ring says it in one glyph, and a clone
 *  card's solid dot is reserved for something that changes while you watch it rather than a
 *  state somebody set in another tool. */
export function TicketCardBody({
  ticket,
  selected = false,
  onCreateClone,
  onCancel,
  onMoveToBacklog,
}: {
  ticket: LinearTicket;
  /** Selected: the panel on the right is showing this ticket. The tint alone says so. No
   *  left accent bar — a card is one object, and a bar down its edge reads as a second one
   *  next to it. */
  selected?: boolean;
  /** Opens the clone dialog for this ticket. Absent ⇒ the item is not offered, which is
   *  what the drag overlay and a board with no columns get. */
  onCreateClone?: () => void;
  onCancel?: () => void;
  onMoveToBacklog?: () => void;
}) {
  const labels = ticket.labels ?? [];
  return (
    <article
      className={`px-2.5 py-2 ${selected ? "bg-emerald-50 dark:bg-emerald-950" : ""}`}
    >
      {/* State, priority, then labels: the row Linear itself draws, in its own marks. No
          owner on it: every ticket here belongs to one of the operator's own API keys, so
          naming them would repeat the same handful of names down the column and answer a
          question nobody asked. */}
      <div className="flex items-start gap-1.5">
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
          <StateIcon state={ticket.state} />
          {ticket.priority ? <PriorityIcon level={ticket.priority} /> : null}
          {labels.map((label) => (
            <LabelPill key={label.name} name={label.name} color={label.color} />
          ))}
        </div>
        {/* The ⋮ button is a 16px icon in a 24px hit area, ten pixels taller than the marks
            it sits beside. Left alone it sets the row's height and opens a gap under the
            marks that reads as the card's own spacing. Pulling its padding out of the flow
            hands the height back to the marks and centres the icon on them. */}
        <div className="-my-1 shrink-0">
          <TicketMenu
            ticket={ticket}
            onCreateClone={onCreateClone}
            onCancel={onCancel}
            onMoveToBacklog={onMoveToBacklog}
          />
        </div>
      </div>

      <p className="mt-1 break-words text-sm font-medium leading-snug text-slate-800 dark:text-slate-100">
        <span
          className={`mr-1 inline-block rounded px-1 py-0.5 align-middle text-[10px] font-semibold leading-none ${workspaceBadge(
            teamOf(ticket),
          )}`}
        >
          {ticket.id}
        </span>
        {ticket.title}
      </p>
    </article>
  );
}

function TicketCard({
  ticket,
  selected = false,
  onSelect,
  onCreateClone,
  onCancel,
  onMoveToBacklog,
}: {
  ticket: LinearTicket;
  selected?: boolean;
  onSelect?: () => void;
  onCreateClone?: () => void;
  onCancel?: () => void;
  onMoveToBacklog?: () => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: ticketDragId(ticket.id),
  });
  const style: CSSProperties = {
    transform: CSS.Translate.toString(transform),
    transition,
    // The original keeps its slot as a hole while the overlay follows the pointer, so the
    // cards below it do not jump twice.
    opacity: isDragging ? 0 : 1,
  };
  return (
    <div
      ref={setNodeRef}
      style={style}
      {...attributes}
      {...listeners}
      onClick={onSelect}
      className="cursor-grab touch-none"
    >
      <CardFrame>
        <TicketCardBody
          ticket={ticket}
          selected={selected}
          onCreateClone={onCreateClone}
          onCancel={onCancel}
          onMoveToBacklog={onMoveToBacklog}
        />
      </CardFrame>
    </div>
  );
}

export interface TicketColumnProps {
  /** Already filtered to open, unclaimed tickets — see `openTickets`. The server keeps
   *  this current, so the column draws it and nothing else. */
  tickets: LinearTicket[];
  /** The ticket whose panel is open, if any. */
  selectedId?: string | null;
  /** A card was clicked. The container opens it in the side panel. */
  onSelectTicket?: (ticket: LinearTicket) => void;
  /** No answer from Linear yet, which is only true before the first one arrives. */
  loading?: boolean;
  /** The server's last poll of Linear failed. The column keeps whatever it had and says so,
   *  because a stale list beats an empty one while the fleet is mid-flight. */
  error?: string | null;
  /** The card menu's "Create a clone…". Absent ⇒ the item is not offered. Where the clone
   *  is filed is the caller's call: this action names no column, unlike a drag, which
   *  names the one it landed on. */
  onCreateClone?: (ticket: LinearTicket) => void;
  /** Set the ticket Cancelled in Linear. The caller drops it from `tickets` on its own:
   *  waiting for the next fetch would leave a card sitting there that has already been
   *  cancelled. */
  onCancel?: (ticket: LinearTicket) => void;
  /** Set the ticket Backlog in Linear. Same deal — the caller drops it. */
  onMoveToBacklog?: (ticket: LinearTicket) => void;
  /** Open the new-ticket dialog. Absent ⇒ no button, which is what a board with no Linear
   *  key configured gets. */
  onNewTicket?: () => void;
}

export function TicketColumn({
  tickets,
  loading = false,
  error = null,
  selectedId = null,
  onSelectTicket,
  onCreateClone,
  onCancel,
  onMoveToBacklog,
  onNewTicket,
}: TicketColumnProps) {
  // The well takes drops so a ticket dragged back into the column lands, including on the
  // empty space below the last card.
  const { setNodeRef } = useDroppable({ id: TICKET_COLUMN_ID });
  const dragIds = tickets.map((t) => ticketDragId(t.id));
  // A first fetch still running is not an empty queue, so it says nothing rather than
  // claiming everything is claimed.
  const emptyLine =
    tickets.length > 0 || loading
      ? null
      : error
        ? "Nothing to show."
        : "Every open ticket has a clone.";

  return (
    <ColumnShell>
      <ColumnHeader>
        <h2 className={COLUMN_TITLE}>Tickets</h2>
        {/* The same button the clone columns carry, in the same place, for the same reason:
            the thing a column is for is the thing you make more of at the top of it. */}
        {onNewTicket ? (
          <button
            type="button"
            onClick={onNewTicket}
            title="Open a new Linear ticket"
            className="flex shrink-0 cursor-pointer items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-slate-500 hover:bg-slate-200 dark:text-slate-400 dark:hover:bg-slate-700"
          >
            <Plus className="size-3.5" />
            New ticket
          </button>
        ) : null}
      </ColumnHeader>

      <ColumnWell innerRef={setNodeRef} empty={emptyLine}>
        {error ? (
          <p className="rounded-lg bg-rose-50 px-2 py-1.5 text-xs text-rose-700 dark:bg-rose-950 dark:text-rose-300">
            {error}
          </p>
        ) : null}
        <SortableContext items={dragIds} strategy={verticalListSortingStrategy}>
          {tickets.map((ticket) => (
            <TicketCard
              key={ticket.id}
              ticket={ticket}
              selected={ticket.id === selectedId}
              onSelect={onSelectTicket ? () => onSelectTicket(ticket) : undefined}
              onCreateClone={onCreateClone ? () => onCreateClone(ticket) : undefined}
              onCancel={onCancel ? () => onCancel(ticket) : undefined}
              onMoveToBacklog={onMoveToBacklog ? () => onMoveToBacklog(ticket) : undefined}
            />
          ))}
        </SortableContext>
      </ColumnWell>
    </ColumnShell>
  );
}
