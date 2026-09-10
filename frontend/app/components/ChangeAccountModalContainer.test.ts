import { expect, test } from "bun:test";

import { currentCodexValue, currentValue } from "./ChangeAccountModalContainer";
import type { Clone } from "~/lib/types";

const clone = (overrides: Partial<Clone> = {}): Clone => ({
  id: "h1",
  host: "h1",
  port: 3389,
  username: "rmng",
  password: "rmng",
  managed: true,
  ...overrides,
});

test("a legacy clone with no selection seeds as auto (no tokenless state)", () => {
  const h = clone();

  expect(currentValue(h)).toBe("auto");
  expect(currentCodexValue(h)).toBe("auto");
});

test("legacy group:/none selections seed as auto, keeping the group", () => {
  const h = clone({ group: "team", claudeSelection: "group:team", codexSelection: "none" });

  expect(currentValue(h)).toBe("auto");
  expect(currentCodexValue(h)).toBe("auto");
});
