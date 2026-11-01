// Moving one thing in a list the operator orders by hand: Move up, Move down and Move to top
// in a sheet, for a page so small that a drag fights the scroll.

/** Where a move sends the item. */
export type Move = "top" | "up" | "down";

/** The index `id` goes to in `ids` for `move`, or null when it is already there (or absent). */
function moveTarget(ids: string[], id: string, move: Move): number | null {
  const from = ids.indexOf(id);
  if (from < 0) return null;
  const to = move === "top" ? 0 : move === "up" ? from - 1 : from + 1;
  if (to < 0 || to >= ids.length || to === from) return null;
  return to;
}

/** `ids` with `id` moved; the same list when it cannot move. */
export function moved(ids: string[], id: string, move: Move): string[] {
  const to = moveTarget(ids, id, move);
  if (to === null) return ids;
  const rest = ids.filter((x) => x !== id);
  rest.splice(to, 0, id);
  return rest;
}

/** The moves that do something for `id` in `ids`. To the top is left out where it is the
 *  same move as up. */
export function movesFor(ids: string[], id: string): Move[] {
  return (["top", "up", "down"] as const).filter(
    (m) => moveTarget(ids, id, m) !== null && !(m === "top" && ids.indexOf(id) === 1),
  );
}
