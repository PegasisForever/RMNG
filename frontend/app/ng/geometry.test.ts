import { describe, expect, test } from "bun:test";

import { GAP, KEY_ASPECT, MIN_BUTTONS_H, TOOL_H, keysLayout, screenFit } from "./geometry";

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
