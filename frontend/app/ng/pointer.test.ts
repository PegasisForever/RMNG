import { expect, test } from "bun:test";

import { clampTo, pointerGain, scrollGain } from "./pointer";

test("a slow finger moves the cursor as far as it moves, a fast one further, up to a limit", () => {
  expect(pointerGain(1, 100)).toBeCloseTo(1.015);
  expect(pointerGain(10, 10)).toBeCloseTo(2.5);
  expect(pointerGain(100, 1)).toBe(4);
  expect(pointerGain(-10, 10)).toBe(pointerGain(10, 10));
});

test("clampTo keeps a point on the monitor", () => {
  expect(clampTo(-5, 100)).toBe(0);
  expect(clampTo(150, 100)).toBe(99);
  expect(clampTo(42.5, 100)).toBe(42.5);
});

test("the scroll follows the layout's speed: the Duo's 15% is 0.3 surface px per CSS px, fingers up scroll down", () => {
  expect(scrollGain(15)).toBeCloseTo(-0.3);
  expect(scrollGain(100)).toBe(-2);
  expect(scrollGain(-15)).toBeCloseTo(0.3);
});
