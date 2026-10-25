import { describe, expect, test } from "bun:test";

import { DEAD_PX, NOTCH_PX, clampTo, dotVelocity, scrollSteps } from "./pointer";

describe("dotVelocity", () => {
  test("a push inside the dead zone does not move the cursor", () => {
    expect(dotVelocity(DEAD_PX, 0)).toEqual({ vx: 0, vy: 0 });
  });

  test("moves in the direction of the push", () => {
    const { vx, vy } = dotVelocity(0, -10);
    expect(vx).toBeCloseTo(0);
    expect(vy).toBeLessThan(0);
  });

  test("a bigger push is faster, more than in proportion", () => {
    const slow = dotVelocity(5, 0).vx;
    const fast = dotVelocity(10, 0).vx;
    expect(fast / slow).toBeGreaterThan(2);
  });

  test("has a top speed", () => {
    expect(dotVelocity(1000, 0).vx).toBe(dotVelocity(2000, 0).vx);
  });
});

test("clampTo keeps a point on the monitor", () => {
  expect(clampTo(-5, 100)).toBe(0);
  expect(clampTo(150, 100)).toBe(99);
  expect(clampTo(42.5, 100)).toBe(42.5);
});

describe("scrollSteps", () => {
  test("a drag down scrolls down, a drag up scrolls up", () => {
    expect(scrollSteps(NOTCH_PX * 2, { y: 0 })).toBe(2);
    expect(scrollSteps(-NOTCH_PX, { y: 0 })).toBe(-1);
  });

  test("keeps part of a notch for the next move", () => {
    const rem = { y: 0 };
    expect(scrollSteps(NOTCH_PX / 2, rem)).toBe(0);
    expect(scrollSteps(NOTCH_PX / 2, rem)).toBe(1);
  });
});
