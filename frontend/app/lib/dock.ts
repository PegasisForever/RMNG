// The pinned columns of each monitor window: the control panel's wide columns the operator
// pinned (the pin in a column's header) stay at the left edge of that monitor's window, in a
// dock, while the control panel is closed. Each monitor of this browser's layout has its own
// pins (`rmng.docks`, beside the layout: `./clientLayout.ts`).
//
// The layout keeps each monitor's full size. The dock takes its width from the clone's screen,
// so what the browser claims is narrower: the monitor less `cut` clone px at its right
// (`claimedMonitors`). The window that shows the monitor works out `cut` (`dockCut`) and keeps it
// here, so any window of the browser can claim the whole layout. `cut` does not follow the
// window's size: the claimed resolution changes when a pin, the pixel scale or the screen's
// pixel ratio changes, not when the window is resized. No React.
import type { MonitorSpec } from "~/lib/wire/MonitorSpec";

import { resize } from "./monitorLayout";

/** The pins: the rail (the server's numbers and the accounts' usage), the tickets, and each
 *  clone column of the board (`columnPin`). */
export const RAIL_PIN = "rail";
export const TICKETS_PIN = "tickets";
export const columnPin = (columnId: string) => `column:${columnId}`;

/** The columns' widths, CSS px: the rail's `w-64`, the tickets' and the clone columns' `w-72`
 *  (`~/desk/control/WideLayout.tsx`, `TicketsView.tsx`, `ClonesView.tsx`). */
export const RAIL_W = 256;
export const COLUMN_W = 288;
/** The gap between the dock's columns, and round them (`gap-2`, `p-2`), CSS px. */
export const DOCK_GAP = 8;

/** The dock's width, CSS px, for `pins` (in `orderPins` order); 0 with none. */
export function dockWidth(pins: readonly string[]): number {
  if (!pins.length) return 0;
  return DOCK_GAP + pins.reduce((w, p) => w + (p === RAIL_PIN ? RAIL_W : COLUMN_W) + DOCK_GAP, 0);
}

/** The pins that name a column there is, in the board's order: the rail, the tickets, then the
 *  clone columns as the board has them (`columnIds`). A removed column's pin is left out. */
export function orderPins(pins: readonly string[], columnIds: readonly string[]): string[] {
  const has = new Set(pins);
  return [RAIL_PIN, TICKETS_PIN, ...columnIds.map(columnPin)].filter((p) => has.has(p));
}

/** The clone's screen keeps at least this width beside the dock, CSS px. */
export const MIN_SCREEN_W = 320;

/** The first of `pins` (in `orderPins` order) whose dock fits in a window `width` CSS px wide,
 *  `inset` of it unsafe at the left, with MIN_SCREEN_W left for the clone's screen. The others
 *  stay pinned, out of the dock until the window is wide enough. */
export function fitPins(pins: readonly string[], width: number, inset = 0): string[] {
  let n = pins.length;
  while (n > 0 && inset + dockWidth(pins.slice(0, n)) > width - MIN_SCREEN_W) n--;
  return pins.slice(0, n);
}

/** `pins` with `pin` added, or taken out when it is there. */
export function togglePin(pins: readonly string[], pin: string): string[] {
  return pins.includes(pin) ? pins.filter((p) => p !== pin) : [...pins, pin];
}

/** A monitor window's dock: the pins, and the clone px it takes from the monitor's width. */
export type Dock = { pins: string[]; cut: number };
export const NO_DOCK: Dock = { pins: [], cut: 0 };

/**
 * The clone px a dock `dockCss` CSS px wide takes from the monitor's width (even: the encoder
 * takes even sizes only): `dockCss × dpr / scale`, with `dpr` the screen's device px per CSS px
 * and `scale` the monitor's pixel scale (device px per clone px). It is the rule Match window
 * sizes a monitor by (the window's device px / the pixel scale), so in a window the monitor
 * matches, the dock and the narrower monitor at its pixel scale fill it side by side. The
 * window's size is not in it: a window resized shows the same monitor larger or smaller, as
 * one with no dock does.
 */
export function dockCut(dockCss: number, dpr: number, scale: number): number {
  if (dockCss <= 0 || !(scale > 0)) return 0;
  return Math.ceil((dockCss * dpr) / scale / 2) * 2;
}

/** The monitors as claimed: each one `docks[i].cut` px narrower, still side by side as before
 *  (`resize`: each keeps the edge it touched). */
export function claimedMonitors(monitors: MonitorSpec[], docks: readonly Dock[]): MonitorSpec[] {
  if (!monitors.some((_, i) => (docks[i]?.cut ?? 0) > 0)) return monitors;
  return resize(
    monitors,
    monitors.map((m, i) => ({ width: m.width - (docks[i]?.cut ?? 0), height: m.height })),
  );
}

// ---- storage ----

const DOCKS_KEY = "rmng.docks";

function isDock(v: unknown): v is Dock {
  const d = v as Dock | null;
  return (
    !!d &&
    Array.isArray(d.pins) &&
    d.pins.every((p) => typeof p === "string") &&
    Number.isFinite(d.cut) &&
    d.cut >= 0
  );
}

/** Each monitor's dock (`[i]` for monitor i; a monitor with none: NO_DOCK). */
export function readDocks(): Dock[] {
  try {
    const raw = JSON.parse(localStorage.getItem(DOCKS_KEY) ?? "[]") as unknown;
    if (!Array.isArray(raw)) return [];
    return raw.map((d) => (isDock(d) ? { pins: d.pins, cut: Math.round(d.cut / 2) * 2 } : NO_DOCK));
  } catch {
    return [];
  }
}

export function readDock(monitor: number): Dock {
  return readDocks()[monitor] ?? NO_DOCK;
}

function writeDocks(docks: Dock[]) {
  try {
    localStorage.setItem(DOCKS_KEY, JSON.stringify(docks));
  } catch {
    // no storage (a private window): the pins last until the page closes
  }
}

/** Keep monitor `monitor`'s dock. */
export function saveDock(monitor: number, dock: Dock) {
  const docks = readDocks();
  for (let i = docks.length; i < monitor; i++) docks[i] = NO_DOCK;
  docks[monitor] = dock;
  writeDocks(docks);
}

/** The docks follow their monitors after the monitors were renumbered (`map[old]` is the new
 *  number; null: removed). */
export function renumberDocks(map: (number | null)[]) {
  const docks = readDocks();
  const out: Dock[] = [];
  map.forEach((to, k) => {
    if (to !== null) out[to] = docks[k] ?? NO_DOCK;
  });
  writeDocks(Array.from(out, (d) => d ?? NO_DOCK));
}
