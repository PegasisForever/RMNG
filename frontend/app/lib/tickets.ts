// Linear tickets that have no clone yet: the board's inbox of work not started.
//
// A Linear API key per preset (`Preset.linear_key`) reaches the browser verbatim as
// `PresetRedacted.linearKey`, and the browser lists the issues itself through
// `~/lib/linear`. This module holds the rules it applies to that list, kept apart from the
// fetch so a story can pass a fixture and the filter stays testable.
//
// Whose tickets: each key is personal, so each one answers for its own owner and the column
// shows the union across every configured key. A fleet with a work key and a personal key
// draws both owners' queues as one list. Nothing on the card says which key a ticket came
// from, because the operator holds all of them and the answer would never change what they
// do next. The union is why `openTickets` deduplicates: two presets can carry the same key,
// and then the same issue arrives twice.

import type { Clone } from "~/lib/types";
import type { LinearTicket, TicketState } from "~/lib/linear/types";

/** The ticket shape belongs to `~/lib/linear/types`, next to the query that fills it in. It
 *  is re-exported here so every consumer imports tickets from one place, and so the fields
 *  the column reads are documented next to the rules that read them. */
export type { LinearTicket } from "~/lib/linear/types";
export type { TicketLabel } from "~/lib/linear/types";
export type { TicketState } from "~/lib/linear/types";
export type { TicketWorkflowState } from "~/lib/linear/types";

/** Drag ids have to be unique across the whole board, and a ticket id could in principle
 *  collide with a clone id. Prefixing keeps the two namespaces apart and lets the drag
 *  handlers tell at a glance which kind of thing is moving. */
const DRAG_PREFIX = "ticket:";

/** The ticket column's own droppable id. Colon-separated so it can never collide with a
 *  board column id, which `newColumnId` builds out of a slugged title. */
export const TICKET_COLUMN_ID = "tickets:column";

export function ticketDragId(ticketId: string): string {
  return `${DRAG_PREFIX}${ticketId}`;
}

/** The ticket id inside a drag id, or null when the drag id belongs to a clone. */
export function ticketIdFromDrag(dragId: string): string | null {
  return dragId.startsWith(DRAG_PREFIX) ? dragId.slice(DRAG_PREFIX.length) : null;
}

/** The git branch to work this ticket on.
 *
 *  Linear's own `branchName` when it sent one, because that is the string its UI copies and
 *  the one its branch-to-issue linking matches on. Otherwise the same shape without the
 *  handle Linear would have prefixed: the identifier, lowercased, then the title slugged.
 *
 *  The slug keeps only ASCII letters and digits, so a title with punctuation or an em dash
 *  in it cannot produce a branch git has to be argued with. It is capped at 48 characters on
 *  a word boundary, which keeps the whole name inside what a terminal prompt shows. */
export function branchNameOf(ticket: LinearTicket): string {
  if (ticket.branchName) return ticket.branchName;
  const slug = ticket.title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48)
    .replace(/-+[^-]*$/, (tail) => (tail.length > 12 ? "" : tail))
    .replace(/-+$/, "");
  return slug ? `${ticket.id.toLowerCase()}-${slug}` : ticket.id.toLowerCase();
}

/** A clone's Linear ticket as Linear has it now, in the two fields a clone card draws.
 *
 *  A clone stores both at creation and never hears about them again, so a ticket renamed in
 *  Linear leaves the clone reading the old title forever. This is the live answer, resolved by
 *  the browser and handed to the card. The stored fields stay, as what the card falls back to
 *  when Linear has not answered. */
export interface CloneTicket {
  title: string;
  url: string;
}

/** Each clone's live ticket, by clone id.
 *
 *  Only the clones whose ticket is in `tickets`: one Linear cannot answer for is absent rather
 *  than blank, which is what lets the card tell "Linear says this" apart from "nobody asked
 *  yet" and fall back for the second. */
export function cloneTickets(
  clones: Clone[],
  tickets: LinearTicket[],
): Record<string, CloneTicket> {
  const byId = new Map(tickets.map((t) => [t.id.toLowerCase(), t]));
  const live: Record<string, CloneTicket> = {};
  for (const clone of clones) {
    const ticket = clone.linearTicket ? byId.get(clone.linearTicket.toLowerCase()) : undefined;
    if (ticket) live[clone.id] = { title: ticket.title, url: ticket.url };
  }
  return live;
}

/** The clone somebody made for `ticketId`, or null.
 *
 *  Archived clones count. The ticket did get a clone, and offering to make a second one
 *  because the first was retired is how you end up with two.
 *
 *  Identifiers are matched case-insensitively throughout this module: the operator types
 *  them into the clone dialog by hand, and `we-301` means the same issue as `WE-301`. */
export function cloneForTicket(ticketId: string, clones: Clone[]): Clone | null {
  const wanted = ticketId.toLowerCase();
  return clones.find((c) => c.linearTicket?.toLowerCase() === wanted) ?? null;
}

/** The ticket with `ticketId` among `tickets`, or null. Pass the list the column actually
 *  draws: a ticket that is not in it has nothing on this board to show. */
export function findTicket(ticketId: string, tickets: LinearTicket[]): LinearTicket | null {
  const wanted = ticketId.toLowerCase();
  return tickets.find((t) => t.id.toLowerCase() === wanted) ?? null;
}

/** Whether a ticket in `state` is work the column shows. It shows what is queued or underway
 *  and nothing else, so a ticket set to any other state leaves the column.
 *
 *  `openTickets` filters on this, and the dashboard asks it before a state write, to know
 *  whether the card it is about to change is one that will still be there afterwards. */
export function queued(state: TicketState): boolean {
  return state === "todo" || state === "in_progress";
}

/** The ticket the column moves to when `ticketId` leaves it: the one below, or the one above
 *  when it was last, or null when it was the only one.
 *
 *  Pass the list as it stands *before* the departure. Marking the open ticket Done is what
 *  this is for: its card goes, and the panel would otherwise sit empty over a queue that
 *  still has work in it. */
export function ticketAfter(tickets: LinearTicket[], ticketId: string): LinearTicket | null {
  const wanted = ticketId.toLowerCase();
  const at = tickets.findIndex((t) => t.id.toLowerCase() === wanted);
  if (at < 0) return null;
  return tickets[at + 1] ?? tickets[at - 1] ?? null;
}

/** The tickets the column draws: open ones nobody has cloned yet, deduplicated, in the
 *  order given. Put `orderTickets` after this to apply the operator's own arrangement.
 *
 *  The first copy of a repeated id wins, so a ticket keeps the position its earliest key
 *  gave it rather than jumping to the end of the merged list. */
export function openTickets(tickets: LinearTicket[], clones: Clone[]): LinearTicket[] {
  const seen = new Set<string>();
  return tickets.filter((t) => {
    if (!queued(t.state)) return false;
    const key = t.id.toLowerCase();
    if (seen.has(key) || cloneForTicket(t.id, clones)) return false;
    seen.add(key);
    return true;
  });
}

/** `tickets` arranged the way the operator left the column, with anything new on top.
 *
 *  The column is a queue the operator owns, not a mirror of Linear. Once a ticket has been
 *  placed it keeps that place, whatever Linear's own view does with it afterwards. A ticket
 *  the stored order has never seen is new, and new work goes to the top where it will be
 *  looked at, rather than to the bottom of a queue nobody scrolls.
 *
 *  Several new ones keep Linear's relative order among themselves, which is the only order
 *  anyone has expressed about them yet. That is a starting position, not a resting one: the
 *  container adopts them through [`adoptTickets`] on the same pass, which is what stops a
 *  later edit in Linear from re-sorting them. Ids in `order` that no longer exist are
 *  ignored, so a stale entry costs nothing and needs no cleanup pass. */
export function orderTickets(tickets: LinearTicket[], order: string[]): LinearTicket[] {
  const rank = new Map(order.map((id, i) => [id.toLowerCase(), i]));
  const fresh: LinearTicket[] = [];
  const placed: LinearTicket[] = [];
  for (const t of tickets) {
    if (rank.has(t.id.toLowerCase())) placed.push(t);
    else fresh.push(t);
  }
  placed.sort((a, b) => rank.get(a.id.toLowerCase())! - rank.get(b.id.toLowerCase())!);
  return [...fresh, ...placed];
}

/** `order` with every ticket it has never seen written in at the top, or null when it already
 *  covers them all.
 *
 *  Without this the stored order only ever held the tickets somebody had dragged, and every
 *  other card fell through to the order Linear sent. That query is `orderBy: updatedAt`, so
 *  editing any ticket in Linear silently re-sorted every un-dragged card in the column. A
 *  queue that rearranges itself when you touch something else is not a queue.
 *
 *  So a ticket takes its place the moment the column first draws it, not the first time it is
 *  dragged. `tickets` arrives from [`orderTickets`], which has already put the new ones on top
 *  in Linear's relative order, and this pins exactly that.
 *
 *  Lowercased, matching what the server stores, so the list this produces and the one that
 *  comes back over SSE are the same array rather than two spellings of it. */
export function adoptTickets(tickets: LinearTicket[], order: string[]): string[] | null {
  const known = new Set(order.map((id) => id.toLowerCase()));
  const fresh: string[] = [];
  for (const t of tickets) {
    const id = t.id.toLowerCase();
    if (known.has(id)) continue;
    known.add(id); // A repeated id is adopted once, however it got into the list.
    fresh.push(id);
  }
  if (fresh.length === 0) return null;
  return [...fresh, ...order.map((id) => id.toLowerCase())];
}
