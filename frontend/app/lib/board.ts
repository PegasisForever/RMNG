// The board's data model: which clone sits in which column, in what order.
//
// A column owns an ordered list of clone ids. Two things are deliberately NOT stored:
// the archived column's contents (derived from `clone.archived`, the server's truth) and
// any clone the operator has not filed yet (it shows up in the first column). Both fall
// out of `resolveColumns`, so the stored list can lag behind the live clone set without
// a clone ever disappearing from the board.

import type { Clone } from "~/lib/types";
import type { BoardColumn } from "~/lib/wire/BoardColumn";

/** The stored column shape, straight off the wire (`ControlState.boardColumns`). Ids of
 *  clones that no longer exist are ignored on render and dropped by the next move. */
export type { BoardColumn };

/** The fixed rightmost column. Not a stored column: dropping a card here archives the
 *  clone, and dragging it out unarchives it. */
export const ARCHIVED_COLUMN_ID = "archived";

/** What the board shows before the operator has made any columns. Without it an empty
 *  column list would leave every clone unfiled with nowhere to land, so a fresh install
 *  would look like it has no clones at all. */
export const DEFAULT_COLUMNS: BoardColumn[] = [
  { id: "clones", title: "Clones", cloneIds: [] },
];

/** The stored columns, or the default one when nothing is stored yet. */
export function withDefaults(columns: BoardColumn[]): BoardColumn[] {
  return columns.length > 0 ? columns : DEFAULT_COLUMNS;
}

/** The columns as drawn: existing, unarchived clones only, with anything unfiled appended
 *  to the first column. Returns `columns` unchanged in shape, so the caller can map over
 *  it directly. */
export function resolveColumns(columns: BoardColumn[], clones: Clone[]): BoardColumn[] {
  const live = new Map(clones.filter((c) => !c.archived).map((c) => [c.id, c]));
  const filed = new Set<string>();
  const out = columns.map((column) => {
    const cloneIds = column.cloneIds.filter((id) => {
      if (!live.has(id) || filed.has(id)) return false;
      filed.add(id);
      return true;
    });
    return { ...column, cloneIds };
  });
  if (out.length === 0) return out;
  const unfiled = [...live.keys()].filter((id) => !filed.has(id));
  out[0] = { ...out[0], cloneIds: [...out[0].cloneIds, ...unfiled] };
  return out;
}

/** Contents of the archived column, in the clone list's own order. */
export function archivedIds(clones: Clone[]): string[] {
  return clones.filter((c) => c.archived).map((c) => c.id);
}

/** The column holding `cloneId`, or null when it is unfiled or archived. */
export function columnIdOf(columns: BoardColumn[], cloneId: string): string | null {
  return columns.find((c) => c.cloneIds.includes(cloneId))?.id ?? null;
}

/** Put `cloneId` at `toIndex` of `toColumnId`, taking it out of wherever it was. Moving it
 *  into the archived column only removes it: that column's contents come from the clone's
 *  own `archived` flag, not from here. */
export function moveCard(
  columns: BoardColumn[],
  cloneId: string,
  toColumnId: string,
  toIndex: number,
): BoardColumn[] {
  const without = columns.map((column) => ({
    ...column,
    cloneIds: column.cloneIds.filter((id) => id !== cloneId),
  }));
  if (toColumnId === ARCHIVED_COLUMN_ID) return without;
  return without.map((column) => {
    if (column.id !== toColumnId) return column;
    const cloneIds = [...column.cloneIds];
    cloneIds.splice(Math.max(0, Math.min(toIndex, cloneIds.length)), 0, cloneId);
    return { ...column, cloneIds };
  });
}

/** Drop a column. Its cards move to the first remaining column rather than falling off the
 *  board, which is also what `resolveColumns` would do with them on the next render. */
export function removeColumn(columns: BoardColumn[], columnId: string): BoardColumn[] {
  const doomed = columns.find((c) => c.id === columnId);
  const rest = columns.filter((c) => c.id !== columnId);
  if (!doomed || rest.length === 0) return rest;
  rest[0] = { ...rest[0], cloneIds: [...rest[0].cloneIds, ...doomed.cloneIds] };
  return rest;
}

/** A column id derived from its title, unique against the columns that already exist. */
export function newColumnId(title: string, columns: BoardColumn[]): string {
  const base = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "column";
  const taken = new Set(columns.map((c) => c.id));
  if (!taken.has(base) && base !== ARCHIVED_COLUMN_ID) return base;
  for (let n = 2; ; n += 1) {
    const candidate = `${base}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}
