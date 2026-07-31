import { expect, test } from "bun:test";

import {
  archivesOnDrop,
  columnIdOf,
  DEFAULT_COLUMNS,
  moveCard,
  newColumnId,
  removeColumn,
  resolveColumns,
  subCloneTree,
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

const sub = (id: string, parent: string): Clone => ({ ...clone(id), parent });

const columns = (): BoardColumn[] => [
  { id: "todo", title: "Todo", cloneIds: ["a", "b"], archive: false },
  { id: "doing", title: "Doing", cloneIds: ["c"], archive: false },
  { id: "done", title: "Done", cloneIds: [], archive: true },
];

test("a clone no column claims lands in the first column", () => {
  const out = resolveColumns(columns(), [clone("a"), clone("b"), clone("c"), clone("d")]);

  expect(out[0].cloneIds).toEqual(["a", "b", "d"]);
  expect(out[1].cloneIds).toEqual(["c"]);
});

test("an unfiled clone that is already archived lands in the first archive column", () => {
  // Dropping it in Todo instead would mean the next drag out of Todo tried to unarchive it.
  const out = resolveColumns(columns(), [clone("a"), clone("z", true)]);

  expect(out[0].cloneIds).toEqual(["a"]);
  expect(out[2].cloneIds).toEqual(["z"]);
});

test("with no archive column an archived clone still shows up rather than vanishing", () => {
  const plain: BoardColumn[] = [{ id: "todo", title: "Todo", cloneIds: [], archive: false }];

  expect(resolveColumns(plain, [clone("z", true)])[0].cloneIds).toEqual(["z"]);
});

test("an archived clone stays in the column it was filed in", () => {
  // The archive flag is a rule about the drop gesture, not a filter on the contents: the card
  // must not move on its own when the server flips `archived`.
  const out = resolveColumns(columns(), [clone("a", true), clone("b")]);

  expect(out[0].cloneIds).toEqual(["a", "b"]);
  expect(out[2].cloneIds).toEqual([]);
});

test("a deleted clone's leftover id is ignored", () => {
  expect(resolveColumns(columns(), [clone("a")])[0].cloneIds).toEqual(["a"]);
});

test("an id filed in two columns is drawn only in the first of them", () => {
  const dup: BoardColumn[] = [
    { id: "todo", title: "Todo", cloneIds: ["a"], archive: false },
    { id: "doing", title: "Doing", cloneIds: ["a", "c"], archive: false },
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

test("a move into an archive column stores the card there like any other", () => {
  // Storing it is what keeps the card still while the archive call is in flight.
  const out = moveCard(columns(), "a", "done", 0);

  expect(out[0].cloneIds).toEqual(["b"]);
  expect(out[2].cloneIds).toEqual(["a"]);
  expect(columnIdOf(out, "a")).toBe("done");
});

test("only a column flagged archive archives what is dropped on it", () => {
  expect(archivesOnDrop(columns(), "done")).toBe(true);
  expect(archivesOnDrop(columns(), "todo")).toBe(false);
  expect(archivesOnDrop(columns(), "nope")).toBe(false);
});

test("deleting a column moves its cards to the first one", () => {
  const out = removeColumn(columns(), "doing");

  expect(out).toHaveLength(2);
  expect(out[0].cloneIds).toEqual(["a", "b", "c"]);
});

test("a fresh install gets somewhere to work and somewhere to archive", () => {
  expect(withDefaults([])).toEqual(DEFAULT_COLUMNS);
  expect(withDefaults([]).some((c) => c.archive)).toBe(true);
  expect(resolveColumns(withDefaults([]), [clone("a")])[0].cloneIds).toEqual(["a"]);
});

test("stored columns win over the defaults", () => {
  expect(withDefaults(columns())).toEqual(columns());
});

test("a new column id is a slug, and never collides with an existing one", () => {
  expect(newColumnId("In Review", columns())).toBe("in-review");
  expect(newColumnId("Todo", columns())).toBe("todo-2");
  expect(newColumnId("!!!", columns())).toBe("column");
});

test("a sub clone hangs under its parent rather than taking a card of its own", () => {
  const { filed, childrenByParent } = subCloneTree([clone("a"), sub("a-1", "a"), clone("b")]);

  expect(filed.map((c) => c.id)).toEqual(["a", "b"]);
  expect(childrenByParent.get("a")?.map((c) => c.id)).toEqual(["a-1"]);
});

test("a sub clone whose parent is gone becomes a card of its own", () => {
  // Dropping it instead would leave a running clone on no board and no way to reach it.
  const { filed, childrenByParent } = subCloneTree([sub("a-1", "a")]);

  expect(filed.map((c) => c.id)).toEqual(["a-1"]);
  expect(childrenByParent.size).toBe(0);
});

test("a column never draws a sub clone, even with its id stored there", () => {
  const stored: BoardColumn[] = [
    { id: "todo", title: "Todo", cloneIds: ["a", "a-1"], archive: false },
  ];
  const out = resolveColumns(stored, [clone("a"), sub("a-1", "a")]);

  expect(out[0].cloneIds).toEqual(["a"]);
});

test("a sub clone is not appended as unfiled either", () => {
  const out = resolveColumns(columns(), [clone("a"), sub("a-1", "a")]);

  expect(out.flatMap((c) => c.cloneIds)).not.toContain("a-1");
});
