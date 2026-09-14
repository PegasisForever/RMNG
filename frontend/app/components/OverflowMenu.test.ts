import { expect, test } from "bun:test";

import { placeMenu } from "./OverflowMenu";

/** A 1440p-ish window, and a 224px-wide panel (`w-56`). */
const VIEWPORT = { width: 1280, height: 800 };
const PANEL = { width: 224, height: 200 };

const trigger = (top: number, right: number) => ({
  top,
  bottom: top + 20,
  left: right - 20,
  right,
});

test("hangs a menu below its trigger when the room is there", () => {
  expect(placeMenu(trigger(100, 600), PANEL, VIEWPORT, "right")).toEqual({
    top: 124,
    left: 376,
    maxHeight: 668,
  });
});

test("flips a menu above its trigger rather than off the bottom of the window", () => {
  // 700px down a 800px window: 76px below, 688px above. The old fixed panel drew to 924.
  const at = placeMenu(trigger(700, 600), PANEL, VIEWPORT, "right");

  expect(at.top).toBe(696 - PANEL.height);
  expect(at.top + PANEL.height).toBeLessThanOrEqual(700);
  expect(at.maxHeight).toBeGreaterThanOrEqual(PANEL.height);
});

test("scrolls a menu that fits neither way, on the roomier side", () => {
  const tall = { width: 224, height: 900 };

  // Near the top: below is roomier, so it opens downward and is capped there.
  const low = placeMenu(trigger(100, 600), tall, VIEWPORT, "right");
  expect(low).toMatchObject({ top: 124, maxHeight: 668 });

  // Near the bottom: above is roomier, and the cap puts its top on the margin.
  const high = placeMenu(trigger(700, 600), tall, VIEWPORT, "right");
  expect(high).toMatchObject({ top: 8, maxHeight: 688 });
});

test("keeps a menu off both side edges whichever way it is aligned", () => {
  // A ⋮ in the leftmost column: right-aligning would put the panel at -114.
  expect(placeMenu(trigger(100, 110), PANEL, VIEWPORT, "right").left).toBe(8);

  // A left-aligned menu near the right edge would run to 1494.
  expect(placeMenu(trigger(100, 1270), PANEL, VIEWPORT, "left").left).toBe(
    VIEWPORT.width - 8 - PANEL.width,
  );
});

test("still places a panel wider than the window it has to fit in", () => {
  const wide = { width: 400, height: 100 };

  expect(placeMenu(trigger(100, 300), wide, { width: 320, height: 800 }, "right").left).toBe(8);
});
