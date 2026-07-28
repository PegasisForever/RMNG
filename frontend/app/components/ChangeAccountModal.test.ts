import { expect, test } from "bun:test";

import { currentCodexValue, currentValue } from "./ChangeAccountModal";
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

test("tokenless legacy Claude clone is not treated as already auto", () => {
  const h = clone();

  expect(currentValue(h)).toBe("none");
  expect("auto" !== currentValue(h)).toBe(true);
});

test("tokenless legacy Codex clone is not treated as already auto", () => {
  const h = clone();

  expect(currentCodexValue(h)).toBe("none");
  expect("auto" !== currentCodexValue(h)).toBe(true);
});
