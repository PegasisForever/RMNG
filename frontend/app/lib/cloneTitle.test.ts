import { describe, expect, test } from "bun:test";

import { cloneTitle } from "./cloneTitle";

const base = { id: "pega-we-142", host: "pega-we-142" } as const;

describe("cloneTitle", () => {
  test("the ticket's live title over the name it was made with", () => {
    expect(cloneTitle({ ...base, displayName: "Old title" }, { title: "New title" })).toBe("New title");
  });
  test("the operator's name over the ticket's title", () => {
    expect(cloneTitle({ ...base, displayName: "Mine", named: true }, { title: "New title" })).toBe("Mine");
  });
  test("no name and no ticket: the hostname", () => {
    expect(cloneTitle({ ...base })).toBe("pega-we-142");
  });
});
