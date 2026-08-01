// Linear tickets that have no clone yet: the board's inbox of work not started.
//
// The server holds a Linear API key per preset (`Preset.linear_key`, redacted to
// `linearKeySet` for the browser), so it is the side that can list issues. This module is
// the shape the browser draws and the one rule it applies, kept apart from the fetch so a
// story can pass a fixture and the filter stays testable.
//
// Whose tickets: each key is personal, so each one answers for its own owner and the column
// shows the union across every configured key. A fleet with a work key and a personal key
// draws both owners' queues as one list. Nothing on the card says which key a ticket came
// from, because the operator holds all of them and the answer would never change what they
// do next. The union is why `openTickets` deduplicates: two presets can carry the same key,
// and then the same issue arrives twice.

import type { Clone } from "~/lib/types";
import type { LinearTicket } from "~/lib/wire/LinearTicket";

/** The ticket shape is the server's, generated from `wire::LinearTicket` by ts-rs. It is
 *  re-exported here so every consumer imports tickets from one place, and so the fields the
 *  column reads are documented next to the rules that read them. */
export type { LinearTicket } from "~/lib/wire/LinearTicket";
export type { TicketLabel } from "~/lib/wire/TicketLabel";
export type { TicketState } from "~/lib/wire/TicketState";

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

/** The tickets the column draws: open ones nobody has cloned yet, deduplicated, in the
 *  order given. Put `orderTickets` after this to apply the operator's own arrangement.
 *
 *  The first copy of a repeated id wins, so a ticket keeps the position its earliest key
 *  gave it rather than jumping to the end of the merged list. */
export function openTickets(tickets: LinearTicket[], clones: Clone[]): LinearTicket[] {
  const seen = new Set<string>();
  return tickets.filter((t) => {
    if (t.state !== "todo" && t.state !== "in_progress") return false;
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
 *  anyone has expressed about them yet. Ids in `order` that no longer exist are ignored, so
 *  a stale entry costs nothing and needs no cleanup pass. */
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
