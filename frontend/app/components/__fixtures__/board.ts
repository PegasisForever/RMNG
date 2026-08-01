// The dashboard board's swim lanes. `legacy-desktop` is deliberately in none of them: an
// unfiled clone shows up in the first column, which is how a new clone reaches the board.
// `pega-we-142-helper` is in none of them either, for the opposite reason: it is a sub clone,
// so the board draws it under its parent rather than filing it anywhere.

import type { BoardColumn } from "~/lib/board";

import { cloneDualProvider, cloneIdle, cloneNoToken, cloneOffline, cloneWorking } from "./clones";

/** An empty operator column. Pass `archive: true` for the retirement lane. */
export function makeBoardColumn(overrides: Partial<BoardColumn> = {}): BoardColumn {
  return { id: "todo", title: "Todo", cloneIds: [], archive: false, ...overrides };
}

export const boardColumns: BoardColumn[] = [
  makeBoardColumn({ id: "todo", title: "Todo", cloneIds: [cloneIdle.id, cloneNoToken.id] }),
  makeBoardColumn({
    id: "doing",
    title: "In progress",
    cloneIds: [cloneWorking.id, cloneDualProvider.id],
  }),
  makeBoardColumn({ id: "blocked", title: "Blocked", cloneIds: [cloneOffline.id] }),
  // Dropping a clone here archives it; dragging one out starts it again.
  makeBoardColumn({ id: "archived", title: "Archived", archive: true }),
];
