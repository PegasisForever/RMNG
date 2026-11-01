import { describe, expect, test } from "bun:test";

import { type ScreenBox, centeredOn, otherScreen } from "./otherScreen";

const screen = (left: number, top: number, width: number, height: number, isPrimary = false): ScreenBox => ({
  left,
  top,
  availLeft: left,
  availTop: top + 25,
  availWidth: width,
  availHeight: height - 25,
  isPrimary,
});

describe("otherScreen", () => {
  const primary = screen(0, 0, 1920, 1080, true);
  const right = screen(1920, 0, 2560, 1440);
  const left = screen(-1440, 0, 1440, 900);

  test("one screen: none", () => {
    expect(otherScreen([primary], primary)).toBeNull();
  });

  test("on the primary screen: the first other", () => {
    expect(otherScreen([primary, right, left], primary)).toBe(right);
  });

  test("on another screen: the primary one", () => {
    expect(otherScreen([right, left, primary], right)).toBe(primary);
  });

  test("the same screen as a new object is not another", () => {
    expect(otherScreen([{ ...primary }], primary)).toBeNull();
  });
});

describe("centeredOn", () => {
  test("in the middle of the free area", () => {
    expect(centeredOn(screen(1920, 0, 2560, 1440), 560, 900)).toEqual({ left: 2920, top: 283, width: 560, height: 900 });
  });

  test("smaller where it does not fit", () => {
    expect(centeredOn(screen(-1440, 0, 1440, 900), 560, 900)).toEqual({ left: -1000, top: 25, width: 560, height: 875 });
  });
});
