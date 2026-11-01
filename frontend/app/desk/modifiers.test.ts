import { beforeEach, describe, expect, test } from "bun:test";

import { DOUBLE_TAP_MS, Modifiers } from "./modifiers";

const CTRL = 29;
const SHIFT = 42;
const ALT = 56;

let sent: string[];
let mods: Modifiers;
beforeEach(() => {
  sent = [];
  mods = new Modifiers(
    (code, pressed) => sent.push(`${code}${pressed ? "↓" : "↑"}`),
    (code) => code === SHIFT,
  );
});

/** A key that is no modifier, tapped. */
function typeKey() {
  mods.keyDown();
  sent.push("key");
  mods.keyDone();
}

const tap = (code: number, t: number) => {
  mods.down(code, t);
  mods.up(code, false, t + 50);
};

describe("Modifiers", () => {
  test("a tapped modifier goes down for the next key, then up: Ctrl, C is Ctrl+C", () => {
    tap(CTRL, 0);
    expect(mods.states().get(CTRL)).toBe("latched");
    typeKey();
    typeKey();
    expect(sent).toEqual(["29↓", "key", "29↑", "key"]);
    expect(mods.states().size).toBe(0);
  });

  test("tapped again before a key, it goes up", () => {
    tap(CTRL, 0);
    tap(CTRL, 1000);
    expect(sent).toEqual(["29↓", "29↑"]);
  });

  test("a held modifier stays down for every key until its finger lifts: Ctrl held, A, D", () => {
    mods.down(CTRL, 0);
    typeKey();
    typeKey();
    expect(mods.states().get(CTRL)).toBe("held");
    mods.up(CTRL, false, 500);
    expect(sent).toEqual(["29↓", "key", "key", "29↑"]);
  });

  test("two tapped modifiers both go up after the key: Ctrl, Alt, T", () => {
    tap(CTRL, 0);
    tap(ALT, 100);
    typeKey();
    expect(sent).toEqual(["29↓", "56↓", "key", "29↑", "56↑"]);
  });

  test("Shift tapped twice quickly locks, stays down through keys, and the next tap unlocks", () => {
    tap(SHIFT, 0);
    tap(SHIFT, 50 + DOUBLE_TAP_MS - 100);
    expect(mods.states().get(SHIFT)).toBe("locked");
    typeKey();
    typeKey();
    tap(SHIFT, 2000);
    expect(sent).toEqual(["42↓", "key", "key", "42↑"]);
  });

  test("two slow taps on Shift do not lock it", () => {
    tap(SHIFT, 0);
    tap(SHIFT, 50 + DOUBLE_TAP_MS + 100);
    expect(sent).toEqual(["42↓", "42↑"]);
  });

  test("Ctrl does not lock", () => {
    tap(CTRL, 0);
    tap(CTRL, 100);
    expect(sent).toEqual(["29↓", "29↑"]);
  });

  test("a locked Shift held while typing stays locked after", () => {
    tap(SHIFT, 0);
    tap(SHIFT, 100);
    mods.down(SHIFT, 1000);
    typeKey();
    mods.up(SHIFT, false, 1200);
    expect(mods.states().get(SHIFT)).toBe("locked");
    expect(sent).toEqual(["42↓", "key"]);
  });

  test("a touch on a modifier that became a drag leaves it as it was", () => {
    mods.down(CTRL, 0);
    mods.up(CTRL, true, 100);
    expect(sent).toEqual(["29↓", "29↑"]);
    tap(ALT, 200);
    mods.down(ALT, 300);
    mods.up(ALT, true, 400);
    expect(mods.states().get(ALT)).toBe("latched");
  });
});
