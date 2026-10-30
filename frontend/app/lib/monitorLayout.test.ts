import { describe, expect, test } from "bun:test";

import { addMonitor, dropMonitor, makePrimary, normalize, removeMonitor, resize } from "./monitorLayout";

const mon = (width: number, height: number, x: number, y: number) => ({ width, height, x, y, primary: false });

describe("normalize", () => {
  test("starts at 0, 0, monitor 0 the only primary one, even sides in range", () => {
    expect(normalize([mon(1921, 100, 500, 40), { ...mon(1920, 1080, 2421, 40), primary: true }])).toEqual([
      { width: 1920, height: 320, x: 0, y: 0, primary: true },
      { width: 1920, height: 1080, x: 1921, y: 0, primary: false },
    ]);
  });
});

describe("dropMonitor", () => {
  const two = normalize([mon(2560, 1440, 0, 0), mon(1920, 1080, 2560, 0)]);

  test("dropped near the left edge, it goes to the left, its top lined up", () => {
    expect(dropMonitor(two, 1, -2000, 60)[1]).toMatchObject({ x: 0, y: 0 });
    expect(dropMonitor(two, 1, -2000, 60)[0]).toMatchObject({ x: 1920, y: 0 });
  });

  test("dropped below, it goes under, kept where it was dropped along the edge", () => {
    const m = dropMonitor(two, 1, 900, 1500);
    expect(m[1]).toMatchObject({ x: 900, y: 1440 });
  });

  test("dropped far off, it still touches along at least 100 px", () => {
    const m = dropMonitor(two, 1, 2560, 5000);
    expect(m[1].y).toBe(1440);
    expect(m[1].x).toBeLessThanOrEqual(2560 - 100);
  });

  test("near the middle of a neighbour's edge, it lines up with its center", () => {
    const m = dropMonitor(two, 1, 2560, 150);
    expect(m[1].y + m[1].height / 2).toBe(m[0].y + m[0].height / 2);
  });

  test("the drop lands on whole pixels", () => {
    const m = dropMonitor(two, 1, 2600.4, 400.7);
    expect(m.every((x) => Number.isInteger(x.x) && Number.isInteger(x.y))).toBe(true);
  });

  test("it never covers a third monitor", () => {
    const three = normalize([mon(1920, 1080, 0, 0), mon(1920, 1080, 1920, 0), mon(1920, 1080, 3840, 0)]);
    const m = dropMonitor(three, 2, 1900, 10);
    for (let i = 0; i < 3; i++)
      for (let j = i + 1; j < 3; j++) {
        const [a, b] = [m[i], m[j]];
        const cover = a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
        expect(cover).toBe(false);
      }
  });
});

describe("resize", () => {
  test("monitors in a row stay in a row, tops lined up", () => {
    const row = normalize([mon(1920, 1080, 0, 0), mon(1920, 1080, 1920, 0), mon(1920, 1080, 3840, 0)]);
    const m = resize(row, [
      { width: 2560, height: 1440 },
      { width: 1000, height: 800 },
      { width: 1920, height: 1080 },
    ]);
    expect(m.map((x) => [x.x, x.y])).toEqual([[0, 0], [2560, 0], [3560, 0]]);
  });

  test("a monitor on the left stays on the left; one under stays under", () => {
    const l = normalize([mon(2560, 1440, 1920, 0), mon(1920, 1080, 0, 360), mon(1000, 800, 1920, 1440)]);
    const m = resize(l, [{ width: 3000, height: 2000 }, l[1], l[2]]);
    // Lined up with the bottom before, so with the bottom now
    expect(m[1].x + m[1].width).toBe(m[0].x);
    expect(m[1].y + m[1].height).toBe(m[0].y + m[0].height);
    expect(m[2].y).toBe(m[0].y + m[0].height);
    expect(m[2].x).toBe(m[0].x);
  });
});

describe("add, remove, primary", () => {
  test("a new monitor goes at the right of the rightmost one", () => {
    const m = addMonitor(normalize([mon(2560, 1440, 0, 0)]));
    expect(m[1]).toEqual({ width: 1920, height: 1080, x: 2560, y: 0, primary: false });
  });

  test("removing the middle of three closes up the row", () => {
    const row = normalize([mon(1920, 1080, 0, 0), mon(1920, 1080, 1920, 0), mon(1000, 800, 3840, 0)]);
    const m = removeMonitor(row, 1);
    expect(m).toHaveLength(2);
    expect(m[1]).toMatchObject({ x: 1920, y: 0 });
  });

  test("the last monitor stays", () => {
    const one = normalize([mon(1920, 1080, 0, 0)]);
    expect(removeMonitor(one, 0)).toBe(one);
  });

  test("a monitor made primary becomes monitor 0 and stays where it is", () => {
    const two = normalize([mon(1920, 1080, 0, 0), mon(2560, 1440, 1920, 0)]);
    const m = makePrimary(two, 1);
    expect(m[0]).toEqual({ width: 2560, height: 1440, x: 1920, y: 0, primary: true });
    expect(m[1]).toEqual({ width: 1920, height: 1080, x: 0, y: 0, primary: false });
  });
});
