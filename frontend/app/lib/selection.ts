// Which item the dashboard has open, kept in the page's address rather than in React state,
// so the browser's own Back button is what returns to the one before it.
//
// Two things can be open at once, so there are two parameters. Opening a ticket does not stop
// the viewer following the clone underneath it, and picking a clone is how a ticket closes,
// so neither can stand in for the other.
//
// Ids are written lowercase and compared lowercase. A clone id is a DNS label and already is
// lowercase; a Linear identifier is typed by hand in several places, and `WE-301` has to name
// the same address as `we-301`.

/** The clone param: which clone's panels are open. */
const CLONE = "clone";
/** The ticket param: which ticket's panel is open. */
const TICKET = "ticket";

export interface Selection {
  /** The open clone's id, lowercase, or null when the address names none. */
  clone: string | null;
  /** The open ticket's identifier, lowercase, or null when the address names none. */
  ticket: string | null;
}

/** The empty address: nothing open. */
export const NO_SELECTION: Selection = { clone: null, ticket: null };

/** One id as the address holds it. A blank or whitespace-only value is no value, which is
 *  what makes `?clone=` mean the same as no parameter at all. */
function readId(raw: string | null): string | null {
  const id = (raw ?? "").trim().toLowerCase();
  return id === "" ? null : id;
}

/** What `params` says is open. */
export function readSelection(params: URLSearchParams): Selection {
  return { clone: readId(params.get(CLONE)), ticket: readId(params.get(TICKET)) };
}

/** `params` with `selection` written into it and every other parameter left alone.
 *
 *  The other parameters matter: the address is the whole page's, not this module's, and a
 *  selection that dropped one would break whatever put it there. */
export function withSelection(params: URLSearchParams, selection: Selection): URLSearchParams {
  const next = new URLSearchParams(params);
  const pairs: [string, string | null][] = [
    [CLONE, selection.clone],
    [TICKET, selection.ticket],
  ];
  for (const [key, value] of pairs) {
    const id = readId(value);
    if (id === null) next.delete(key);
    else next.set(key, id);
  }
  return next;
}

/** Whether two selections name the same two things. Guards the write: pushing an address the
 *  page is already at leaves a history entry that Back steps through and nothing changes. */
export function sameSelection(a: Selection, b: Selection): boolean {
  return a.clone === b.clone && a.ticket === b.ticket;
}
