// The draft store is what carries unsent composer text across a clone switch, since
// ChatPanel is keyed by clone id and remounts. These cases pin the two properties that
// makes that work: drafts are per clone, and an emptied box leaves nothing behind.
import { beforeEach, expect, test } from "bun:test";

import { clearDrafts, getDraft, setDraft } from "./chatDrafts";

beforeEach(() => clearDrafts());

test("an unknown clone has an empty draft", () => {
  expect(getDraft("never-typed-in")).toBe("");
});

test("drafts are kept per clone", () => {
  setDraft("dev-1", "restart the build");
  setDraft("dev-2", "check the logs");

  expect(getDraft("dev-1")).toBe("restart the build");
  expect(getDraft("dev-2")).toBe("check the logs");
});

test("a later write replaces the earlier draft for that clone", () => {
  setDraft("dev-1", "resta");
  setDraft("dev-1", "restart the build");

  expect(getDraft("dev-1")).toBe("restart the build");
});

test("emptying the box drops the draft and leaves other clones alone", () => {
  setDraft("dev-1", "restart the build");
  setDraft("dev-2", "check the logs");

  setDraft("dev-1", "");

  expect(getDraft("dev-1")).toBe("");
  expect(getDraft("dev-2")).toBe("check the logs");
});
