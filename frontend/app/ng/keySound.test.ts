import { describe, expect, test } from "bun:test";

import { keyRows } from "./keyboard";
import { clickKind, renderClick } from "./keySound";

const find = (id: string) => [...keyRows(false), ...keyRows(true)].flat().find((k) => k.id === id)!;

describe("clickKind", () => {
  test("letters, digits and symbols tick; Delete and Backspace have their own; wide keys and modifiers the low one", () => {
    expect(["KeyA", "Digit5", "Comma", "ArrowLeft"].map((id) => clickKind(find(id)))).toEqual(["key", "key", "key", "key"]);
    expect(["Backspace", "Delete"].map((id) => clickKind(find(id)))).toEqual(["delete", "delete"]);
    expect(["Space", "Enter", "ShiftLeft", "Tab", "ControlLeft", "Escape", "F1"].map((id) => clickKind(find(id)))).toEqual(
      Array(7).fill("modifier"),
    );
  });
});

describe("renderClick", () => {
  test("a short click: loud at the start, silent by the end, no pop on the first sample", () => {
    for (const kind of ["key", "delete", "modifier"] as const) {
      const s = renderClick(kind, 48000);
      const peak = Math.max(...s.map(Math.abs));
      expect(peak).toBeGreaterThan(0.5);
      expect(peak).toBeLessThanOrEqual(0.6 + 1e-6);
      expect(Math.abs(s[0])).toBeLessThan(0.01);
      const tail = s.slice(-480);
      expect(Math.max(...tail.map(Math.abs))).toBeLessThan(0.01);
    }
  });
});
