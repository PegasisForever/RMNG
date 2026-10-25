import { describe, expect, test } from "bun:test";

import { MIN_KEYS, MIN_SCREEN, clampSplit, defaultSplit } from "./split";

// The unfolded iPhone Duo: 951 px tall, an 82 px clock bar, a 34 px home bar.
const duo = { height: 951, top: 82, bottom: 34 };

describe("clampSplit", () => {
  test("keeps a line between the two least heights where it is", () => {
    expect(clampSplit(500, duo)).toBe(500);
  });

  test("keeps the screen area at least MIN_SCREEN under the clock bar", () => {
    expect(clampSplit(0, duo)).toBe(82 + MIN_SCREEN);
  });

  test("keeps the keyboard area at least MIN_KEYS above the home bar", () => {
    expect(clampSplit(951, duo)).toBe(951 - 34 - MIN_KEYS);
  });

  test("rounds to whole pixels", () => {
    expect(clampSplit(500.6, duo)).toBe(501);
  });

  test("gives the screen area its least height when both cannot fit", () => {
    expect(clampSplit(100, { height: 300, top: 82, bottom: 34 })).toBe(82 + MIN_SCREEN);
  });
});

test("defaultSplit gives the screen area a bit more than half under the clock bar", () => {
  expect(defaultSplit(duo)).toBe(Math.round(82 + (951 - 82) * 0.55));
});
