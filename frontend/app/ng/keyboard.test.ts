import { describe, expect, test } from "bun:test";

import { ROW_UNITS, keyLabel, keyRows } from "./keyboard";

const find = (fn: boolean, id: string) => keyRows(fn).flat().find((k) => k.id === id)!;

describe("keyRows", () => {
  for (const fn of [false, true]) {
    test(`every row is ${ROW_UNITS} units wide (fn ${fn})`, () => {
      for (const row of keyRows(fn)) expect(row.reduce((s, k) => s + k.units, 0)).toBe(ROW_UNITS);
    });

    test(`no key is in a row set twice (fn ${fn})`, () => {
      const ids = keyRows(fn).flat().map((k) => k.id);
      expect(new Set(ids).size).toBe(ids.length);
    });
  }

  test("the number row reads Esc ` 1 … 0 - = ⌫", () => {
    expect(keyRows(false)[0].map((k) => k.label).join(" ")).toBe("Esc ` 1 2 3 4 5 6 7 8 9 0 - = ⌫");
  });

  test("Fn swaps the number row for F1 to F12 and the arrows for Home PgUp PgDn End", () => {
    const [top, , , , bottom] = keyRows(true);
    expect(top.map((k) => k.label).join(" ")).toBe("Esc F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 Del ⌫");
    expect(bottom.slice(-4).map((k) => k.label)).toEqual(["Home", "PgUp", "PgDn", "End"]);
  });

  test("keys send their evdev codes", () => {
    expect(find(false, "KeyA").code).toBe(30);
    expect(find(true, "F1").code).toBe(59);
    expect(find(false, "MetaLeft").code).toBe(125);
  });

  test("only Shift, Ctrl, Alt and Win are modifiers", () => {
    const mods = keyRows(false).flat().filter((k) => k.mod).map((k) => k.id);
    expect(mods.sort()).toEqual(
      ["AltLeft", "AltRight", "ControlLeft", "ControlRight", "MetaLeft", "ShiftLeft", "ShiftRight"].sort(),
    );
  });
});

describe("keyLabel", () => {
  test("Shift gives the shifted symbols", () => {
    expect(keyLabel(find(false, "Digit1"), true, false)).toBe("!");
    expect(keyLabel(find(false, "Comma"), true, false)).toBe("<");
    expect(keyLabel(find(false, "Backslash"), true, false)).toBe("|");
    expect(keyLabel(find(false, "Comma"), false, false)).toBe(",");
  });

  test("Shift or Caps Lock makes letters upper case, both make them lower case", () => {
    const a = find(false, "KeyA");
    expect(keyLabel(a, false, false)).toBe("a");
    expect(keyLabel(a, true, false)).toBe("A");
    expect(keyLabel(a, false, true)).toBe("A");
    expect(keyLabel(a, true, true)).toBe("a");
  });

  test("Caps Lock does not change symbols", () => {
    expect(keyLabel(find(false, "Digit1"), false, true)).toBe("1");
  });
});
