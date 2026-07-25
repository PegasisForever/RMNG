import { expect, test } from "bun:test";

import { currentValue } from "./ChangeAccountModal";
import type { Clone } from "~/lib/types";
import type { Group } from "~/lib/wire/Group";

const clone = (overrides: Partial<Clone> = {}): Clone => ({
  id: "h1",
  host: "h1",
  port: 3389,
  username: "rmng",
  password: "rmng",
  managed: true,
  group: "pooled",
  ...overrides,
});

const groups: Group[] = [{ name: "Default" }, { name: "pooled" }];

test("a clone bound to a group reads back its group name", () => {
  expect(currentValue(clone(), groups)).toBe("pooled");
});

test("a blank binding falls back to the first group", () => {
  // Every clone binds a group, but a row written before that rule reads blank off the wire
  // until the server's normalizer repoints it.
  expect(currentValue(clone({ group: "" }), groups)).toBe("Default");
});

test("no groups yet (config still loading) yields an empty value", () => {
  // The Apply button is disabled on empty, so this can't submit a blank binding.
  expect(currentValue(clone({ group: "" }), [])).toBe("");
});
