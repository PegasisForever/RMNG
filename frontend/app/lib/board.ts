// The board's data model: which clone sits in which column, in what order.
//
// Column membership is the only thing that decides where a card is drawn. That is what keeps
// a card still while an archive or unarchive round-trips: the server's `archived` flag is the
// *effect* of dropping a clone into an archive column, never the input to placement. Deriving
// placement from the flag instead would move the card twice on every drop, once back to where
// it came from while the call was in flight and once forward when the answer arrived.
//
// A clone the operator has not filed anywhere is not stored at all; `resolveColumns` gives it
// a home so a newly created clone is visible without anyone writing it down first.
//
// A sub clone is never filed. It is drawn under whichever card its parent is on, so giving it
// a column of its own would draw it twice and let a drag separate it from the parent that
// spawned it. `subCloneTree` is the one place that rule lives.

import type { Clone } from "~/lib/types";
import type { BoardColumn } from "~/lib/wire/BoardColumn";

/** The stored column shape, straight off the wire (`ControlState.boardColumns`). */
export type { BoardColumn };

/** What the board shows before the operator has made any columns: somewhere to work and
 *  somewhere to retire clones to. Without it an empty column list would leave every clone
 *  unfiled with nowhere to land, so a fresh install would look like it has no clones. */
export const DEFAULT_COLUMNS: BoardColumn[] = [
  { id: "clones", title: "Clones", cloneIds: [], archive: false },
  { id: "archived", title: "Archived", cloneIds: [], archive: true },
];

/** The stored columns, or the defaults when nothing is stored yet. */
export function withDefaults(columns: BoardColumn[]): BoardColumn[] {
  return columns.length > 0 ? columns : DEFAULT_COLUMNS;
}

/** Where an unfiled clone goes: the first archive column if it is already archived, else the
 *  first column. An archived clone landing in an ordinary column would be unarchived by the
 *  next drag out of it, which is not what anyone means by "I never filed this one". */
function homeFor(columns: BoardColumn[], clone: Clone): string | undefined {
  if (clone.archived) {
    const archive = columns.find((c) => c.archive);
    if (archive) return archive.id;
  }
  return columns[0]?.id;
}

/** Clones split into the ones the board files on its own and each parent's sub clones, both
 *  keeping the incoming order.
 *
 *  A sub clone whose parent is gone — archived out of the list, or deleted — is promoted to a
 *  card of its own rather than dropped, because the alternative is a running clone that has
 *  vanished from the board with no way to reach it. */
export function subCloneTree(clones: Clone[]): {
  filed: Clone[];
  childrenByParent: Map<string, Clone[]>;
} {
  const ids = new Set(clones.map((c) => c.id));
  const filed: Clone[] = [];
  const childrenByParent = new Map<string, Clone[]>();
  for (const clone of clones) {
    if (clone.parent && ids.has(clone.parent)) {
      const siblings = childrenByParent.get(clone.parent) ?? [];
      siblings.push(clone);
      childrenByParent.set(clone.parent, siblings);
    } else {
      filed.push(clone);
    }
  }
  return { filed, childrenByParent };
}

/** The columns as drawn: existing, non-sub clones only, with anything unfiled appended to its
 *  home column. Returns `columns` unchanged in shape, so the caller can map over it directly. */
export function resolveColumns(columns: BoardColumn[], clones: Clone[]): BoardColumn[] {
  const live = new Map(subCloneTree(clones).filed.map((c) => [c.id, c]));
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
  for (const clone of live.values()) {
    if (filed.has(clone.id)) continue;
    const home = homeFor(out, clone);
    const column = out.find((c) => c.id === home);
    if (column) column.cloneIds = [...column.cloneIds, clone.id];
  }
  return out;
}

/** The column holding `cloneId`, or null when it is unfiled. */
export function columnIdOf(columns: BoardColumn[], cloneId: string): string | null {
  return columns.find((c) => c.cloneIds.includes(cloneId))?.id ?? null;
}

/** Whether dropping a clone into `columnId` should archive it. */
export function archivesOnDrop(columns: BoardColumn[], columnId: string): boolean {
  return columns.find((c) => c.id === columnId)?.archive === true;
}

/** Put `cloneId` at `toIndex` of `toColumnId`, taking it out of wherever it was. */
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
  if (!taken.has(base)) return base;
  for (let n = 2; ; n += 1) {
    const candidate = `${base}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}
