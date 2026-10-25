import { describe, expect, test } from "bun:test";

import { liftFor } from "./systemKeyboard";

// The Duo: the page ends at the home bar's top, 951 − 34
const BOTTOM = 917;

describe("liftFor", () => {
  test("with no keyboard the whole screen is in view: no lift", () => {
    expect(liftFor(BOTTOM, 0, 951)).toBe(0);
  });

  test("a keyboard 400 px tall: the screen moves up until the page ends at its top", () => {
    expect(liftFor(BOTTOM, 0, 551)).toBe(366);
  });

  test("iOS scrolled the page for the text box itself: the lift is that much less", () => {
    expect(liftFor(BOTTOM, 100, 551)).toBe(266);
  });
});
