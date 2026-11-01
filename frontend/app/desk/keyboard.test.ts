import { describe, expect, test } from "bun:test";

import { ROW_UNITS, keyLabel, keyRows } from "./keyboard";

const find = (fn: boolean, id: string) => keyRows(fn).flat().find((k) => k.id === id)!;

describe("keyRows", () => {
  for (const fn of [false, true]) {
    test(`every row is ${ROW_UNITS} units wide but the Tab row (fn ${fn})`, () => {
      const widths = keyRows(fn).map((row) => row.reduce((s, k) => s + k.units, 0));
      // The Tab row (the third) is half a unit short: its Tab is one unit
      expect(widths[2]).toBe(ROW_UNITS - 0.5);
      for (const w of [...widths.slice(0, 2), ...widths.slice(3)]) expect(w).toBeCloseTo(ROW_UNITS, 9);
    });

    test(`no key is in a row set twice (fn ${fn})`, () => {
      const ids = keyRows(fn).flat().map((k) => k.id);
      expect(new Set(ids).size).toBe(ids.length);
    });
  }

  test("the number row, second, reads Esc 1 … 0 ⌫", () => {
    expect(keyRows(false)[1].map((k) => k.label).join(" ")).toBe("` 1 2 3 4 5 6 7 8 9 0 ⌫");
  });

  test("the symbols beside the letters and the numbers have a row of their own, on top", () => {
    const rows = keyRows(false);
    expect(rows[0].map((k) => k.label).join(" ")).toBe("Esc [ ] \\ ; ' , . / - = PrtSc");
    expect(rows[0].length).toBe(rows[1].length);
    expect(rows[0].at(-1)!.code).toBe(99); // KEY_SYSRQ, the evdev code of Print Screen
    // No symbol among the letters: only letters and named keys (⌫ is Backspace)
    for (const row of rows.slice(2, 5)) {
      expect(row.every((k) => k.letter || k.label.length > 1 || k.label === "⌫")).toBe(true);
    }
  });

  test("Fn puts F1 to F12 on the symbol row, and Home PgUp PgDn End and the arrows on number keys", () => {
    const [top, numbers, , , , bottom] = keyRows(true);
    expect(top.map((k) => k.label).join(" ")).toBe("F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12");
    expect(numbers.map((k) => k.label).join(" ")).toBe("` Home PgUp PgDn End 5 6 ← ↑ ↓ → ⌫");
    // The keys stay where they are: the same count in each row as without Fn
    expect(keyRows(true).map((r) => r.length)).toEqual(keyRows(false).map((r) => r.length));
    expect(bottom).toEqual(keyRows(false)[5]);
  });

  test("after Space: a second comma, dictation, then Fn", () => {
    const row = keyRows(false)[5];
    expect(row.map((k) => k.id)).toEqual(["ControlLeft", "MetaLeft", "AltLeft", "Space", "CommaRight", "Dictate", "Fn"]);
    expect(row[4].code).toBe(find(false, "Comma").code);
    expect(row[5]).toMatchObject({ dictate: true, code: 0, units: 1 });
    expect(find(false, "Space").units).toBe(5.5);
  });

  test("keys send their evdev codes", () => {
    expect(find(false, "KeyA").code).toBe(30);
    expect(find(true, "F1").code).toBe(59);
    expect(find(false, "MetaLeft").code).toBe(125);
  });

  test("only Shift, Ctrl, Alt and Win are modifiers", () => {
    const mods = keyRows(false).flat().filter((k) => k.mod).map((k) => k.id);
    expect(mods.sort()).toEqual(["AltLeft", "ControlLeft", "MetaLeft", "ShiftLeft"].sort());
  });

  test("Fn is last on the space row, in both modes, and sends nothing", () => {
    for (const fn of [false, true]) {
      const last = keyRows(fn)[5].at(-1)!;
      expect([last.id, last.code]).toEqual(["Fn", 0]);
    }
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

  test("after m is a delete key: Backspace, and forward Delete under Fn", () => {
    const after = (fn: boolean) => keyRows(fn)[4].at(-1)!;
    expect([after(false).code, after(false).label, after(false).units]).toEqual([14, "⌫", 2.25]);
    expect([after(true).code, after(true).label]).toEqual([111, "Del"]);
    // Its own id, unlike the number row's key with the same code
    expect(keyRows(false).flat().filter((k) => k.code === 14).map((k) => k.id)).toEqual(["Backspace", "BackspaceRight"]);
  });
});
