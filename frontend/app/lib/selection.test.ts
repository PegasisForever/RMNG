import { describe, expect, it } from "bun:test";

import { NO_SELECTION, readSelection, sameSelection, withSelection } from "./selection";

describe("readSelection", () => {
  it("reads nothing out of an empty address", () => {
    expect(readSelection(new URLSearchParams())).toEqual(NO_SELECTION);
  });

  it("reads both parameters", () => {
    expect(readSelection(new URLSearchParams("clone=pega-we-1&ticket=we-1"))).toEqual({
      clone: "pega-we-1",
      ticket: "we-1",
    });
  });

  it("lowercases a hand-typed identifier", () => {
    expect(readSelection(new URLSearchParams("ticket=WE-301")).ticket).toBe("we-301");
  });

  it("treats a blank parameter as no parameter", () => {
    expect(readSelection(new URLSearchParams("clone=&ticket=%20"))).toEqual(NO_SELECTION);
  });
});

describe("withSelection", () => {
  it("writes both parameters", () => {
    const next = withSelection(new URLSearchParams(), { clone: "pega-we-1", ticket: "WE-1" });
    expect(next.get("clone")).toBe("pega-we-1");
    expect(next.get("ticket")).toBe("we-1");
  });

  it("drops the parameter a null clears", () => {
    const next = withSelection(new URLSearchParams("clone=a&ticket=we-1"), {
      clone: "a",
      ticket: null,
    });
    expect(next.get("ticket")).toBeNull();
    expect(next.get("clone")).toBe("a");
  });

  it("leaves every other parameter alone", () => {
    const next = withSelection(new URLSearchParams("theme=dark&clone=a"), NO_SELECTION);
    expect(next.get("theme")).toBe("dark");
    expect(next.get("clone")).toBeNull();
  });

  it("does not mutate what it was given", () => {
    const before = new URLSearchParams("clone=a");
    withSelection(before, { clone: "b", ticket: null });
    expect(before.get("clone")).toBe("a");
  });

  it("round-trips through readSelection", () => {
    const selection = { clone: "pega-we-1", ticket: "we-2" };
    expect(readSelection(withSelection(new URLSearchParams(), selection))).toEqual(selection);
  });
});

describe("sameSelection", () => {
  it("is true for the same two ids", () => {
    expect(sameSelection({ clone: "a", ticket: null }, { clone: "a", ticket: null })).toBe(true);
  });

  it("is false when only the ticket moved", () => {
    expect(sameSelection({ clone: "a", ticket: "we-1" }, { clone: "a", ticket: null })).toBe(false);
  });

  it("is false when only the clone moved", () => {
    expect(sameSelection({ clone: "a", ticket: null }, { clone: "b", ticket: null })).toBe(false);
  });
});
