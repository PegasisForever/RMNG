import { describe, expect, test } from "bun:test";

import { GAP, KEY_ASPECT, MIN_BUTTONS_H, TOOL_H, cursorShift, keysLayout, phoneMonitorSize, screenFit } from "./geometry";

// The unfolded iPhone Duo, upright: 669 px wide, a 412 px keyboard area, a 34 px home bar.
const W = 669;
const H = 412;
const BOTTOM = 34;

describe("keysLayout", () => {
  const k = keysLayout(W, H, BOTTOM);
  const full = k.pitch - GAP;

  test("on the Duo the keys take the width", () => {
    expect(k.keys.width).toBeCloseTo(W - 8, 5);
    expect(full).toBeGreaterThan(53);
  });

  test("412 px is too short for six square rows: the four lower stay square, the symbol and number rows give way", () => {
    expect(k.rowHeights.slice(2)).toEqual([full, full, full, full]);
    expect(k.rowHeights[0]).toBe(k.rowHeights[1]);
    expect(k.rowHeights[0]).toBeLessThan(full);
    expect(k.rowHeights[0]).toBeGreaterThan(40);
  });

  test("the tool row is at the top, the keys right under it, the mouse buttons below with 44 px at least", () => {
    expect(k.tools.top).toBe(4);
    expect(k.keys.top).toBe(4 + TOOL_H + GAP);
    expect(k.buttons.top).toBeCloseTo(k.keys.top + k.keys.height + GAP, 9);
    expect(k.buttons.top + k.buttons.height).toBe(H);
    expect(k.buttons.height).toBeGreaterThanOrEqual(MIN_BUTTONS_H - 1e-9);
    expect(k.buttons.width).toBe(W - 8);
  });

  test("an area with room keeps every row square", () => {
    const t = keysLayout(W, 444, BOTTOM);
    expect(t.rowHeights.every((h) => Math.abs(h - full * KEY_ASPECT) < 1e-9)).toBe(true);
  });

  test("a much shorter area makes the lower rows give way too, once the top two are at their least", () => {
    const s = keysLayout(W, 300, BOTTOM);
    expect(s.rowHeights[0]).toBe(24);
    expect(s.rowHeights[2]).toBeLessThan(full);
    expect(s.keys.width).toBeCloseTo(W - 8, 5);
  });
});

describe("screenFit", () => {
  test("the screen area of the iPhone Duo layout shows the monitor at exactly 2×", () => {
    const f = screenFit(669, 539, 1002, 808, 3);
    expect(f.scale).toBe(2);
    expect(f.width).toBeCloseTo(668, 5);
    expect(f.height).toBeCloseTo(538.667, 2);
  });

  test("a taller area letterboxes: bars above and below", () => {
    const f = screenFit(669, 600, 1002, 808, 3);
    expect(f.scale).toBe(2);
    expect(f.top).toBeCloseTo((600 - 808 * 2 / 3) / 2, 5);
  });

  test("a shorter area scales the monitor down to fit, in the middle", () => {
    const f = screenFit(669, 400, 1002, 808, 3);
    expect(f.height).toBeCloseTo(400, 5);
    expect(f.left).toBeCloseTo((669 - f.width) / 2, 5);
  });

  test("never larger than the area", () => {
    for (const h of [300, 537, 539, 600, 800]) {
      const f = screenFit(669, h, 1002, 808, 3);
      expect(f.width).toBeLessThanOrEqual(669 + 1e-9);
      expect(f.height).toBeLessThanOrEqual(h + 1e-9);
    }
  });
});

describe("keysLayout bare (the floating keyboard)", () => {
  test("keys only: from the top, down to the home bar, no tool row, no mouse buttons", () => {
    const k = keysLayout(951, 290, 21, true);
    expect(k.keys.top).toBe(4);
    expect(k.tools.height).toBe(0);
    expect(k.keys.top + k.keys.height).toBeCloseTo(290 - 4 - 21, 5);
  });
});

describe("phoneMonitorSize", () => {
  test("2 device px per monitor px, even: the Duo upright is the Duo layout's size, sideways the same scale", () => {
    expect(phoneMonitorSize(669, 539, 3)).toEqual({ width: 1002, height: 808 });
    expect(phoneMonitorSize(951, 648, 3)).toEqual({ width: 1426, height: 972 });
  });
});

describe("cursorShift", () => {
  test("no shift while the cursor is high, then the screen follows it, up to the screen's bottom", () => {
    expect(cursorShift(100, 400, 648, 40)).toBe(0);
    expect(cursorShift(500, 400, 648, 40)).toBe(140);
    expect(cursorShift(640, 400, 648, 40)).toBe(248);
  });
});

describe("screenFit with a largest scale", () => {
  test("the Duo layout's monitor sideways stays at 2x, in the middle, not stretched to fill", () => {
    const f = screenFit(951, 648, 1002, 808, 3, 2);
    expect(f.scale).toBe(2);
    expect(f.width).toBeCloseTo(668, 0);
    expect(f.left).toBeCloseTo((951 - 668) / 2, 0);
  });

  test("one too large for the screen at 2x still shrinks to fit (the folded phone)", () => {
    expect(screenFit(466, 657, 1002, 808, 3, 2).scale).toBeLessThan(2);
  });
});
