import { expect, test } from "bun:test";

import { matchScreens } from "./open";

const screen = (left: number, top: number) => ({
  left,
  top,
  availLeft: left,
  availTop: top,
  availWidth: 1920,
  availHeight: 1080,
});

test("monitors pair with screens in left-to-right order", () => {
  const monitors = [
    { id: 0, x: 1920, y: 0 },
    { id: 1, x: 0, y: 0 },
  ];
  // Chrome lists the primary screen first; it is on the right here.
  const right = screen(0, 0);
  const left = screen(-1920, 200);
  const m = matchScreens(monitors, [right, left]);
  expect(m.get(1)).toBe(left);
  expect(m.get(0)).toBe(right);
});

test("a stacked layout pairs top to bottom, and extra monitors get no screen", () => {
  const monitors = [
    { id: 0, x: 0, y: 1080 },
    { id: 1, x: 0, y: 0 },
    { id: 2, x: 1920, y: 0 },
  ];
  const top = screen(0, 0);
  const bottom = screen(0, 1080);
  const m = matchScreens(monitors, [bottom, top]);
  expect(m.get(1)).toBe(top);
  expect(m.get(0)).toBe(bottom);
  expect(m.has(2)).toBe(false);
});
