import { expect, test } from "bun:test";

import { currentValue } from "./ChangeAccountModal";
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

test("a clone with no group binding reads as 'none'", () => {
  const h = clone();

  expect(currentValue(h)).toBe("none");
  expect("none" !== currentValue(h)).toBe(false);
});

test("a clone bound to a group reads back its group name", () => {
  const h = clone({ group: "pooled" });

  expect(currentValue(h)).toBe("pooled");
});
