import { describe, expect, test } from "bun:test";

import { GAP, KEY_ASPECT, followAxis, naturalKeysHeight, keysLayout, phoneMonitorSize, screenFit } from "./geometry";

describe("keysLayout", () => {
  // A large phone, upright: 669 px wide, a 34 px home bar
  const k = keysLayout(669, 428, 34);
  const full = k.pitch - GAP;

  test("on a 669 px wide phone the keys take the width", () => {
    expect(k.keys.width).toBeCloseTo(669 - 8, 5);
    expect(full).toBeGreaterThan(53);
  });

  test("the natural height makes every row square; a taller area every row taller alike", () => {
    const n = keysLayout(669, naturalKeysHeight(669, 34), 34);
    expect(n.rowHeights.every((h) => Math.abs(h - full * KEY_ASPECT) < 1e-9)).toBe(true);
    const t = keysLayout(669, naturalKeysHeight(669, 34) + 60, 34);
    expect(t.rowHeights.every((h) => Math.abs(h - (full + 10)) < 1e-9)).toBe(true);
  });

  test("a shorter area: the four lower rows stay square, the symbol and number rows give way", () => {
    const t = keysLayout(669, 380, 34);
    expect(t.rowHeights.slice(2)).toEqual([full, full, full, full]);
    expect(t.rowHeights[0]).toBe(t.rowHeights[1]);
    expect(t.rowHeights[0]).toBeLessThan(full);
  });

  test("a much shorter area makes the lower rows give way too, once the top two are at their least", () => {
    const s = keysLayout(669, 260, 34);
    expect(s.rowHeights[0]).toBe(24);
    expect(s.rowHeights[2]).toBeLessThan(full);
    expect(s.keys.width).toBeCloseTo(669 - 8, 5);
  });

  test("the keys go from the top down to the home bar", () => {
    const f = keysLayout(951, 290, 21);
    expect(f.keys.top).toBe(4);
    expect(f.keys.top + f.keys.height).toBeCloseTo(290 - 4 - 21, 5);
  });
});

describe("screenFit", () => {
  test("a 1002 × 808 monitor in a 669 × 539 area at 3× shows at exactly 2×", () => {
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

describe("phoneMonitorSize", () => {
  test("2 device px per monitor px, even, upright and sideways", () => {
    expect(phoneMonitorSize(669, 539, 3)).toEqual({ width: 1002, height: 808 });
    expect(phoneMonitorSize(951, 648, 3)).toEqual({ width: 1426, height: 972 });
  });
});

describe("screenFit with a largest scale", () => {
  test("a small monitor stays at 2x, in the middle, not stretched to fill", () => {
    const f = screenFit(951, 648, 1002, 808, 3, 2);
    expect(f.scale).toBe(2);
    expect(f.width).toBeCloseTo(668, 0);
    expect(f.left).toBeCloseTo((951 - 668) / 2, 0);
  });

  test("one too large for the screen at 2x still shrinks to fit (the folded phone)", () => {
    expect(screenFit(466, 657, 1002, 808, 3, 2).scale).toBeLessThan(2);
  });
});

describe("followAxis", () => {
  test("a screen shorter than the display stays in the middle while the cursor is in view", () => {
    expect(followAxis(0, 100, 539, 648, 648, 32)).toBeCloseTo(54.5);
  });

  test("with the keyboard up it moves up only as far as the cursor needs, and no further than its bottom", () => {
    expect(followAxis(54.5, 100, 539, 356, 648, 32)).toBeCloseTo(54.5);
    expect(followAxis(54.5, 400, 539, 356, 648, 32)).toBeCloseTo(356 - 32 - 400);
    expect(followAxis(54.5, 539, 539, 356, 648, 32)).toBeCloseTo(356 - 539);
  });

  test("zoomed in (longer than the display) it follows the cursor both ways, between its ends", () => {
    // 1900 long on 951: start between -949 and 0
    expect(followAxis(0, 500, 1900, 951, 951, 32)).toBe(0);
    expect(followAxis(0, 1200, 1900, 951, 951, 32)).toBeCloseTo(951 - 32 - 1200);
    expect(followAxis(-400, 300, 1900, 951, 951, 32)).toBeCloseTo(32 - 300);
    expect(followAxis(-400, 1890, 1900, 951, 951, 32)).toBeCloseTo(951 - 1900);
  });
});

describe("followAxis under the floating buttons", () => {
  // A screen as tall as the display (951), the buttons' row down to 60
  test("the cursor under the buttons moves the screen down, its start no lower than the row's end", () => {
    expect(followAxis(0, 300, 951, 951, 951, 32, 60)).toBe(0);
    expect(followAxis(0, 40, 951, 951, 951, 32, 60)).toBeCloseTo(60 + 32 - 40);
    expect(followAxis(0, 0, 951, 951, 951, 32, 60)).toBe(60);
  });

  test("it comes back up with the cursor as the cursor leaves the row, then rests where it was", () => {
    expect(followAxis(60, 70, 951, 951, 951, 32, 60)).toBeCloseTo(60 + 32 - 70);
    expect(followAxis(22, 100, 951, 951, 951, 32, 60)).toBe(0);
    expect(followAxis(60, 500, 951, 951, 951, 32, 60)).toBe(0);
  });

  test("a screen in the middle with room above it does not move", () => {
    expect(followAxis(0, 0, 539, 648, 648, 32, 40)).toBeCloseTo(54.5);
  });
});

describe("phoneMonitorSize at another scale", () => {
  test("1 device px per monitor px: the display's own pixels", () => {
    expect(phoneMonitorSize(951, 648, 3, 1)).toEqual({ width: 2852, height: 1944 });
  });
});
