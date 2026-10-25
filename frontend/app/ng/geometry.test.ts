import { describe, expect, test } from "bun:test";

import { GAP, TOOL_H, keysLayout, naturalKeysHeight, screenFit } from "./geometry";

// The unfolded iPhone Duo, upright: 669 px wide, a 34 px home bar.
const W = 669;
const BOTTOM = 34;

describe("keysLayout", () => {
  test("at the natural height the keys are square and as wide as the screen allows", () => {
    const k = keysLayout(W, naturalKeysHeight(W, BOTTOM), BOTTOM);
    expect(k.width).toBeCloseTo(W - 8, 0);
    expect(k.height).toBeCloseTo((k.pitch * 5 - GAP), 5);
    expect(k.top + k.height).toBeCloseTo(naturalKeysHeight(W, BOTTOM) - BOTTOM, 5);
  });

  test("a taller area keeps the keys' size and puts them at the bottom", () => {
    const natural = keysLayout(W, naturalKeysHeight(W, BOTTOM), BOTTOM);
    const tall = keysLayout(W, 500, BOTTOM);
    expect(tall.pitch).toBeCloseTo(natural.pitch, 5);
    expect(tall.top + tall.height).toBe(500 - BOTTOM);
  });

  test("a shorter area makes the keys smaller, still square, in the middle", () => {
    const k = keysLayout(W, 200, BOTTOM);
    expect(k.width).toBeLessThan(W - 8);
    expect(k.left).toBeCloseTo((W - k.width) / 2, 5);
    expect(k.top).toBeGreaterThanOrEqual(4 + TOOL_H + GAP - 0.001);
  });
});

describe("screenFit", () => {
  test("the screen area of the iPhone Duo layout shows the monitor at exactly 2×", () => {
    const f = screenFit(669, 541, 1002, 810, 3);
    expect(f.scale).toBe(2);
    expect(f.width).toBeCloseTo(668, 5);
    expect(f.height).toBe(540);
  });

  test("a taller area letterboxes: full width, bars above and below", () => {
    const f = screenFit(669, 655, 1002, 810, 3);
    expect(f.scale).toBe(2);
    expect(f.top).toBeCloseTo((655 - 540) / 2, 5);
  });

  test("a shorter area scales the monitor down to fit, in the middle", () => {
    const f = screenFit(669, 400, 1002, 810, 3);
    expect(f.height).toBeCloseTo(400, 5);
    expect(f.width).toBeLessThan(669);
    expect(f.left).toBeCloseTo((669 - f.width) / 2, 5);
  });

  test("never larger than the area", () => {
    for (const h of [300, 539, 541, 600, 800]) {
      const f = screenFit(669, h, 1002, 810, 3);
      expect(f.width).toBeLessThanOrEqual(669 + 1e-9);
      expect(f.height).toBeLessThanOrEqual(h + 1e-9);
    }
  });
});
