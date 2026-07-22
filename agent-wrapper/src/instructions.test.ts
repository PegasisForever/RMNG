import { expect, test } from "bun:test";
import { resolveSystemAppend, BAKED_IN_INSTRUCTIONS } from "./instructions";

test("baked-in default is the merged instructions file, trimmed and non-empty", () => {
  expect(BAKED_IN_INSTRUCTIONS.length).toBeGreaterThan(0);
  expect(BAKED_IN_INSTRUCTIONS).toBe(BAKED_IN_INSTRUCTIONS.trim());
});

test("baked-in instructions leave activity detection to the control server", () => {
  expect(BAKED_IN_INSTRUCTIONS).toContain("proxy token activity");
  expect(BAKED_IN_INSTRUCTIONS).not.toContain("wait-for-stuck");
  expect(BAKED_IN_INSTRUCTIONS).not.toContain("report-detection");
  expect(BAKED_IN_INSTRUCTIONS).not.toContain("set_state");
});

test("a non-empty injected file wins over the baked-in default", () => {
  const injected = "# Custom playbook\nDo the custom thing.";
  const read = () => injected;
  expect(resolveSystemAppend("/any/path", read)).toBe(injected);
});

test("a missing/unreadable file falls back to the baked-in default", () => {
  const read = () => {
    throw new Error("ENOENT");
  };
  expect(resolveSystemAppend("/nope", read)).toBe(BAKED_IN_INSTRUCTIONS);
});

test("an empty/whitespace injected file falls back to the baked-in default", () => {
  expect(resolveSystemAppend("/x", () => "   \n  ")).toBe(BAKED_IN_INSTRUCTIONS);
});
