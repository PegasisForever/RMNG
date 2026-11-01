import { describe, expect, test } from "bun:test";

import { moved, movesFor } from "./order";

const ids = ["a", "b", "c", "d"];

describe("moved", () => {
  test("up and down swap with the neighbour", () => {
    expect(moved(ids, "c", "up")).toEqual(["a", "c", "b", "d"]);
    expect(moved(ids, "b", "down")).toEqual(["a", "c", "b", "d"]);
  });

  test("to top puts it first and keeps the rest in order", () => {
    expect(moved(ids, "d", "top")).toEqual(["d", "a", "b", "c"]);
  });

  test("a move off either end changes nothing", () => {
    expect(moved(ids, "a", "up")).toBe(ids);
    expect(moved(ids, "d", "down")).toBe(ids);
    expect(moved(ids, "x", "up")).toBe(ids);
  });
});

describe("movesFor", () => {
  test("the first has only down, the last has top and up, one alone has none", () => {
    expect(movesFor(ids, "a")).toEqual(["down"]);
    expect(movesFor(ids, "d")).toEqual(["top", "up"]);
    expect(movesFor(["a"], "a")).toEqual([]);
  });

  test("for the second, to the top is the same as up, so only up is offered", () => {
    expect(movesFor(ids, "b")).toEqual(["up", "down"]);
    expect(movesFor(ids, "c")).toEqual(["top", "up", "down"]);
  });
});
