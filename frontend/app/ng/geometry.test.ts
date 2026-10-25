import { describe, expect, test } from "bun:test";

import { GAP, KEY_ASPECT, TOOL_H, keysLayout, screenFit } from "./geometry";

// The unfolded iPhone Duo, upright: 669 px wide, a 444 px keyboard area, a 34 px home bar.
const W = 669;
const H = 444;
const BOTTOM = 34;

describe("keysLayout", () => {
  const k = keysLayout(W, H, BOTTOM);

  test("on the Duo the keys take the width and are square", () => {
    expect(k.keys.width).toBeCloseTo(W - 8, 5);
    expect(k.pitch - GAP).toBeGreaterThan(53);
    expect(k.keyHeight).toBeCloseTo((k.pitch - GAP) * KEY_ASPECT, 9);
    expect(KEY_ASPECT).toBe(1);
  });

  test("the tool row is at the top, the keys right under it, the space left at the bottom", () => {
    expect(k.tools.top).toBe(4);
    expect(k.keys.top).toBe(4 + TOOL_H + GAP);
    expect(k.keys.top + k.keys.height).toBeLessThan(H - BOTTOM);
  });

  test("the trackpad takes everything under the tool row, down to the keys' bottom", () => {
    expect(k.pad.top).toBe(4 + TOOL_H + GAP);
    expect(k.pad.top + k.pad.height).toBeCloseTo(H - BOTTOM, 9);
    expect(k.pad.width).toBe(W - 8);
  });

  test("a short area makes the keys shorter, still the whole width", () => {
    const s = keysLayout(W, 250, BOTTOM);
    expect(s.keys.width).toBeCloseTo(W - 8, 5);
    expect(s.keyHeight).toBeLessThan(k.keyHeight);
    expect(s.keys.top).toBeCloseTo(4 + TOOL_H + GAP, 9);
  });
});

describe("screenFit", () => {
  test("the screen area of the iPhone Duo layout shows the monitor at exactly 2×", () => {
    const f = screenFit(669, 507, 1002, 760, 3);
    expect(f.scale).toBe(2);
    expect(f.width).toBeCloseTo(668, 5);
    expect(f.height).toBeCloseTo(506.667, 2);
  });

  test("a taller area letterboxes: bars above and below", () => {
    const f = screenFit(669, 600, 1002, 760, 3);
    expect(f.scale).toBe(2);
    expect(f.top).toBeCloseTo((600 - 760 * 2 / 3) / 2, 5);
  });

  test("a shorter area scales the monitor down to fit, in the middle", () => {
    const f = screenFit(669, 400, 1002, 760, 3);
    expect(f.height).toBeCloseTo(400, 5);
    expect(f.left).toBeCloseTo((669 - f.width) / 2, 5);
  });

  test("never larger than the area", () => {
    for (const h of [300, 505, 507, 600, 800]) {
      const f = screenFit(669, h, 1002, 760, 3);
      expect(f.width).toBeLessThanOrEqual(669 + 1e-9);
      expect(f.height).toBeLessThanOrEqual(h + 1e-9);
    }
  });
});
