import { expect, test } from "bun:test";

import {
  ARCHIVED_COLUMN_ID,
  archivedIds,
  columnIdOf,
  DEFAULT_COLUMNS,
  moveCard,
  newColumnId,
  removeColumn,
  resolveColumns,
  withDefaults,
  type BoardColumn,
} from "./board";
import type { Clone } from "./types";

const clone = (id: string, archived = false): Clone => ({
  id,
  host: "10.99.0.1",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  archived,
});

const columns = (): BoardColumn[] => [
  { id: "todo", title: "Todo", cloneIds: ["a", "b"] },
  { id: "doing", title: "Doing", cloneIds: ["c"] },
];

test("a clone no column claims lands in the first column", () => {
  const out = resolveColumns(columns(), [clone("a"), clone("b"), clone("c"), clone("d")]);

  expect(out[0].cloneIds).toEqual(["a", "b", "d"]);
  expect(out[1].cloneIds).toEqual(["c"]);
});

test("an archived clone leaves the columns and shows up in the archived list", () => {
  const clones = [clone("a"), clone("b", true), clone("c")];

  expect(resolveColumns(columns(), clones)[0].cloneIds).toEqual(["a"]);
  expect(archivedIds(clones)).toEqual(["b"]);
});

test("a deleted clone's leftover id is ignored", () => {
  // The column list is persisted, so it outlives the clone until the next move rewrites it.
  expect(resolveColumns(columns(), [clone("a")])[0].cloneIds).toEqual(["a"]);
});

test("an id filed in two columns is drawn only in the first of them", () => {
  const dup: BoardColumn[] = [
    { id: "todo", title: "Todo", cloneIds: ["a"] },
    { id: "doing", title: "Doing", cloneIds: ["a", "c"] },
  ];
  const out = resolveColumns(dup, [clone("a"), clone("c")]);

  expect(out[0].cloneIds).toEqual(["a"]);
  expect(out[1].cloneIds).toEqual(["c"]);
});

test("a move takes the card out of its old column and inserts it at the drop index", () => {
  const out = moveCard(columns(), "a", "doing", 1);

  expect(out[0].cloneIds).toEqual(["b"]);
  expect(out[1].cloneIds).toEqual(["c", "a"]);
  expect(columnIdOf(out, "a")).toBe("doing");
});

test("a move within one column reorders it", () => {
  expect(moveCard(columns(), "a", "todo", 1)[0].cloneIds).toEqual(["b", "a"]);
});

test("an index past the end appends rather than dropping the card", () => {
  expect(moveCard(columns(), "a", "doing", 99)[1].cloneIds).toEqual(["c", "a"]);
});

test("a move into the archived column only removes the card", () => {
  const out = moveCard(columns(), "a", ARCHIVED_COLUMN_ID, 0);

  expect(out[0].cloneIds).toEqual(["b"]);
  expect(out[1].cloneIds).toEqual(["c"]);
  expect(columnIdOf(out, "a")).toBeNull();
});

test("deleting a column moves its cards to the first one", () => {
  const out = removeColumn(columns(), "doing");

  expect(out).toHaveLength(1);
  expect(out[0].cloneIds).toEqual(["a", "b", "c"]);
});

test("deleting the first column hands its cards to the next", () => {
  expect(removeColumn(columns(), "todo")[0].cloneIds).toEqual(["c", "a", "b"]);
});

test("a fresh install falls back to one default column", () => {
  // Without this a clone would have no column to be unfiled into, so a new install would
  // render an empty board despite having clones.
  expect(withDefaults([])).toEqual(DEFAULT_COLUMNS);
  expect(resolveColumns(withDefaults([]), [clone("a")])[0].cloneIds).toEqual(["a"]);
});

test("stored columns win over the default", () => {
  expect(withDefaults(columns())).toEqual(columns());
});

test("a new column id is a slug, and never collides with an existing one", () => {
  expect(newColumnId("In Review", columns())).toBe("in-review");
  expect(newColumnId("Todo", columns())).toBe("todo-2");
  // "archived" is the fixed column's id, so a user column can never take it.
  expect(newColumnId("archived", columns())).toBe("archived-2");
  expect(newColumnId("!!!", columns())).toBe("column");
});
