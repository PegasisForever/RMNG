import { beforeEach, describe, expect, test } from "bun:test";

import {
  claimedMonitors,
  COLUMN_W,
  columnPin,
  DOCK_GAP,
  dockCut,
  dockWidth,
  fitPins,
  MIN_SCREEN_W,
  NO_DOCK,
  orderPins,
  RAIL_PIN,
  RAIL_W,
  readDock,
  readDocks,
  renumberDocks,
  saveDock,
  TICKETS_PIN,
  togglePin,
} from "./dock";
import { screenFit } from "~/desk/geometry";

const mon = (width: number, height: number, x: number, y: number, primary = false) => ({ width, height, x, y, primary });

describe("dockWidth", () => {
  test("no pins: no dock", () => {
    expect(dockWidth([])).toBe(0);
  });
  test("the columns' widths, with a gap between them and round them", () => {
    expect(dockWidth([RAIL_PIN])).toBe(DOCK_GAP + RAIL_W + DOCK_GAP);
    expect(dockWidth([TICKETS_PIN, columnPin("a")])).toBe(DOCK_GAP + 2 * (COLUMN_W + DOCK_GAP));
  });
});

describe("orderPins", () => {
  test("the board's order, and no pin of a column that is gone", () => {
    const pins = [columnPin("b"), TICKETS_PIN, columnPin("gone"), RAIL_PIN, columnPin("a")];
    expect(orderPins(pins, ["a", "b"])).toEqual([RAIL_PIN, TICKETS_PIN, columnPin("a"), columnPin("b")]);
  });
});

describe("fitPins", () => {
  const three = [RAIL_PIN, TICKETS_PIN, columnPin("a")];
  test("all of them where the window has room", () => {
    expect(fitPins(three, 2560)).toEqual(three);
  });
  test("the last ones left out where the clone's screen would be under MIN_SCREEN_W", () => {
    // 8 + 264 + 296 = 568 for two, 864 for three
    expect(fitPins(three, 568 + MIN_SCREEN_W)).toEqual([RAIL_PIN, TICKETS_PIN]);
    expect(fitPins(three, 568 + MIN_SCREEN_W - 1)).toEqual([RAIL_PIN]);
    expect(fitPins(three, 100)).toEqual([]);
  });
  test("the unsafe inset at the left counts", () => {
    expect(fitPins(three, 864 + MIN_SCREEN_W, 10)).toEqual([RAIL_PIN, TICKETS_PIN]);
  });
});

describe("togglePin", () => {
  test("adds a pin, and takes it out again", () => {
    expect(togglePin([RAIL_PIN], TICKETS_PIN)).toEqual([RAIL_PIN, TICKETS_PIN]);
    expect(togglePin([RAIL_PIN, TICKETS_PIN], RAIL_PIN)).toEqual([TICKETS_PIN]);
  });
});

describe("dockCut", () => {
  test("1× pixel ratio, 1× scale: as many clone px as CSS px, even", () => {
    expect(dockCut(200, 1, 1)).toBe(200);
    expect(dockCut(305, 1, 1)).toBe(306);
  });
  test("a 2× screen at pixel scale 2: one clone px per CSS px", () => {
    expect(dockCut(200, 2, 2)).toBe(200);
  });
  test("a 2× screen at pixel scale 1: two clone px per CSS px", () => {
    expect(dockCut(200, 2, 1)).toBe(400);
  });
  test("a 1.5× screen at pixel scale 1.25", () => {
    expect(dockCut(200, 1.5, 1.25)).toBe(240);
  });
  test("no dock, or no scale yet: nothing", () => {
    expect(dockCut(0, 2, 1)).toBe(0);
    expect(dockCut(200, 2, 0)).toBe(0);
  });

  // The clone's screen right of the dock keeps the scale the full monitor had in the window,
  // whatever the window's shape
  const keepsScale = (winW: number, winH: number, monW: number, monH: number, dpr: number, scale: number, dock: number) => {
    const before = screenFit(winW, winH, monW, monH, dpr, scale);
    const cut = dockCut(dock, dpr, before.scale);
    const after = screenFit(winW - dock, winH, monW - cut, monH, dpr, scale);
    // The cut is rounded up to even px: at most 2 clone px less wide
    expect(Math.abs(after.scale - before.scale)).toBeLessThan(before.scale * (2 / (monW - cut)) + 1e-9);
    expect(after.width).toBeLessThanOrEqual(winW - dock + 1e-9);
  };
  test("the window is the monitor's size", () => keepsScale(1920, 1080, 1920, 1080, 1, 1, 304));
  test("a 2× screen, pixel scale 2", () => keepsScale(1920, 1080, 1920, 1080, 2, 2, 304));
  test("the window is narrower than the monitor (the monitor fits by its width)", () =>
    keepsScale(1500, 1080, 1920, 1080, 1, 1, 304));
  test("the window is shorter than the monitor (the monitor fits by its height)", () =>
    keepsScale(1920, 800, 1920, 1080, 1, 1, 304));
  test("the window is larger than the monitor (black round it)", () => keepsScale(2560, 1440, 1920, 1080, 1, 1, 304));
  test("a 1.25× screen, pixel scale 1.5, a window of another shape", () => keepsScale(1700, 1000, 2400, 1300, 1.25, 1.5, 600));
});

describe("claimedMonitors", () => {
  const two = [mon(1920, 1080, 0, 0, true), mon(2560, 1440, 1920, 0)];

  test("no dock: the monitors as they are", () => {
    expect(claimedMonitors(two, [])).toBe(two);
    expect(claimedMonitors(two, [NO_DOCK, NO_DOCK])).toBe(two);
  });
  test("monitor 0 is narrower; monitor 1 stays beside it", () => {
    expect(claimedMonitors(two, [{ pins: [RAIL_PIN], cut: 272 }])).toEqual([
      mon(1648, 1080, 0, 0, true),
      mon(2560, 1440, 1648, 0),
    ]);
  });
  test("monitor 1 is narrower; it stays at monitor 0's right", () => {
    expect(claimedMonitors(two, [NO_DOCK, { pins: [RAIL_PIN], cut: 400 }])).toEqual([
      mon(1920, 1080, 0, 0, true),
      mon(2160, 1440, 1920, 0),
    ]);
  });
  test("a monitor never goes under the server's smallest side", () => {
    expect(claimedMonitors([mon(800, 600, 0, 0, true)], [{ pins: [RAIL_PIN], cut: 700 }])[0].width).toBe(320);
  });
});

describe("storage", () => {
  beforeEach(() => {
    // happy-dom's, where another test file registered it; else one of its own
    if (typeof globalThis.localStorage === "undefined") {
      const store = new Map<string, string>();
      Object.defineProperty(globalThis, "localStorage", {
        configurable: true,
        value: {
          getItem: (k: string) => store.get(k) ?? null,
          setItem: (k: string, v: string) => void store.set(k, v),
          removeItem: (k: string) => void store.delete(k),
          clear: () => store.clear(),
        },
      });
    }
    localStorage.clear();
  });

  test("each monitor keeps its own dock", () => {
    saveDock(1, { pins: [TICKETS_PIN], cut: 296 });
    expect(readDock(0)).toEqual(NO_DOCK);
    expect(readDock(1)).toEqual({ pins: [TICKETS_PIN], cut: 296 });
    saveDock(0, { pins: [RAIL_PIN], cut: 272 });
    expect(readDocks()).toEqual([
      { pins: [RAIL_PIN], cut: 272 },
      { pins: [TICKETS_PIN], cut: 296 },
    ]);
  });
  test("junk reads as no dock", () => {
    localStorage.setItem("rmng.docks", JSON.stringify([{ pins: "x", cut: 2 }, null, { pins: [RAIL_PIN], cut: -4 }]));
    expect(readDocks()).toEqual([NO_DOCK, NO_DOCK, NO_DOCK]);
    localStorage.setItem("rmng.docks", "{");
    expect(readDocks()).toEqual([]);
  });
  test("the docks follow their monitors when they are renumbered", () => {
    saveDock(0, { pins: [RAIL_PIN], cut: 272 });
    saveDock(2, { pins: [TICKETS_PIN], cut: 296 });
    // Monitor 1 removed: 2 becomes 1
    renumberDocks([0, null, 1]);
    expect(readDocks()).toEqual([
      { pins: [RAIL_PIN], cut: 272 },
      { pins: [TICKETS_PIN], cut: 296 },
    ]);
    // Monitor 1 made the primary one
    renumberDocks([1, 0]);
    expect(readDock(0)).toEqual({ pins: [TICKETS_PIN], cut: 296 });
    expect(readDock(1)).toEqual({ pins: [RAIL_PIN], cut: 272 });
  });
});
