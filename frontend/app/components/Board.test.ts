import { expect, test } from "bun:test";

import { arrange } from "./Board";

type Lane = { id: string; cloneIds: string[] };

const lanes = (): Lane[] => [
  { id: "todo", cloneIds: ["a", "b"] },
  { id: "doing", cloneIds: ["c"] },
];

// `arrange` returning its input unchanged is what stops the drag preview re-rendering, which
// is what stops dnd-kit re-measuring, which is what stops "Maximum update depth exceeded".
// These pin the identity, not just the contents.

test("a hover that changes nothing returns the very same array", () => {
  const before = lanes();

  expect(arrange(before, "a", "a")).toBe(before);
});

test("a hover over an unknown target returns the very same array", () => {
  const before = lanes();

  expect(arrange(before, "a", "nope")).toBe(before);
  expect(arrange(before, "nope", "b")).toBe(before);
});

test("a hover over the card's own column returns the very same array", () => {
  // The column id is not a card id, so there is no index to move to.
  const before = lanes();

  expect(arrange(before, "a", "todo")).toBe(before);
});

test("a reorder within one lane moves the card and leaves the others alone", () => {
  const before = lanes();
  const after = arrange(before, "a", "b");

  expect(after[0].cloneIds).toEqual(["b", "a"]);
  expect(after[1]).toBe(before[1]);
});

test("a move across lanes takes the card out of one and into the other", () => {
  const after = arrange(lanes(), "a", "c");

  expect(after[0].cloneIds).toEqual(["b"]);
  expect(after[1].cloneIds).toEqual(["a", "c"]);
});

test("a move onto another lane's body appends to it", () => {
  const after = arrange(lanes(), "a", "doing");

  expect(after[0].cloneIds).toEqual(["b"]);
  expect(after[1].cloneIds).toEqual(["c", "a"]);
});
