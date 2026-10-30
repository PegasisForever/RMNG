// The arrangement of a layout's monitors, as the Edit layout dialog changes it: monitor 0 is the
// primary one; each monitor touches another along an edge; the arrangement starts at 0, 0. No
// React, no storage: `./clientLayout.ts` keeps the layout, `~/components/LayoutEditorView.tsx`
// draws it.
import type { MonitorSpec } from "~/lib/wire/MonitorSpec";

/** A monitor's side, in pixels: even (the encoder takes even sizes only) and in this range,
 *  as the server makes it (`sane_layout`). */
export const MIN_SIDE = 320;
export const MAX_SIDE = 8192;
/** The most monitors a layout may have (the server's MAX_MONITORS). */
export const MAX_MONITORS = 8;
/** Two monitors side by side share at least this much of an edge, pixels. */
const MIN_OVERLAP = 100;
/** A dropped monitor this close (part of its side) to lining up with a neighbour (start
 *  edges, end edges, or centers) lines up with it. */
const ALIGN_PART = 0.1;

/** A side as the server takes it. */
export function sane(side: number): number {
  return Math.min(MAX_SIDE, Math.max(MIN_SIDE, Math.round(side))) & ~1;
}

/** The arrangement moved to start at 0, 0, in whole pixels, monitor 0 the only primary one,
 *  every side sane. */
export function normalize(monitors: MonitorSpec[]): MonitorSpec[] {
  if (!monitors.length) return [];
  const sized = monitors.map((m) => ({
    ...m,
    x: Math.round(m.x),
    y: Math.round(m.y),
    width: sane(m.width),
    height: sane(m.height),
  }));
  const x0 = Math.min(...sized.map((m) => m.x));
  const y0 = Math.min(...sized.map((m) => m.y));
  return sized.map((m, i) => ({ ...m, x: m.x - x0, y: m.y - y0, primary: i === 0 }));
}

type Box = { x: number; y: number; width: number; height: number };

function overlaps(a: Box, b: Box): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

/** Which side of `b` monitor `a` is on, when they share a length of edge; else null. */
function sideOf(a: Box, b: Box): "right" | "left" | "below" | "above" | null {
  const shareY = Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y) > 0;
  const shareX = Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x) > 0;
  if (shareY && a.x === b.x + b.width) return "right";
  if (shareY && a.x + a.width === b.x) return "left";
  if (shareX && a.y === b.y + b.height) return "below";
  if (shareX && a.y + a.height === b.y) return "above";
  return null;
}

/** `v` within [lo, hi]; lo wins where the range is empty. */
const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/** Where along a neighbour's edge a monitor goes: `at` (its start) kept so the two share at
 *  least MIN_OVERLAP, then lined up with the neighbour (start edges, end edges, or centers,
 *  whichever is nearest) when close. */
function along(at: number, size: number, nStart: number, nSize: number): number {
  const overlap = Math.min(MIN_OVERLAP, size, nSize);
  const v = clamp(at, nStart - size + overlap, nStart + nSize - overlap);
  const lines = [nStart, nStart + nSize - size, Math.round(nStart + (nSize - size) / 2)];
  const best = lines.reduce((a, b) => (Math.abs(b - v) < Math.abs(a - v) ? b : a));
  return Math.abs(best - v) < size * ALIGN_PART ? best : Math.round(v);
}

/** Where monitor `i` goes when it is dragged to `x`, `y`: the nearest place where it touches
 *  another monitor along an edge, lined up with it when close, and covers none. Null: nowhere
 *  (or no other monitor). */
export function snapMonitor(monitors: MonitorSpec[], i: number, x: number, y: number): { x: number; y: number } | null {
  const m = monitors[i];
  let best: { x: number; y: number; d: number } | null = null;
  monitors.forEach((n, j) => {
    if (j === i) return;
    const ay = along(y, m.height, n.y, n.height);
    const ax = along(x, m.width, n.x, n.width);
    for (const c of [
      { x: n.x + n.width, y: ay },
      { x: n.x - m.width, y: ay },
      { x: ax, y: n.y + n.height },
      { x: ax, y: n.y - m.height },
    ]) {
      const box = { ...c, width: m.width, height: m.height };
      if (monitors.some((o, k) => k !== i && overlaps(box, o))) continue;
      const d = Math.hypot(c.x - x, c.y - y);
      if (!best || d < best.d) best = { ...c, d };
    }
  });
  return best && { x: (best as { x: number }).x, y: (best as { y: number }).y };
}

/** The arrangement after monitor `i` was dropped at `x`, `y` (`snapMonitor`). Nowhere to go:
 *  as before. */
export function dropMonitor(monitors: MonitorSpec[], i: number, x: number, y: number): MonitorSpec[] {
  const at = snapMonitor(monitors, i, x, y);
  return normalize(at ? monitors.map((o, k) => (k === i ? { ...o, ...at } : o)) : monitors);
}

/** The monitors with new sizes (`sizes[i]` for monitor i), arranged as before: each keeps the
 *  side of the monitor it touched, and the edge it lined up with. */
export function resize(monitors: MonitorSpec[], sizes: { width: number; height: number }[]): MonitorSpec[] {
  const next: MonitorSpec[] = monitors.map((m, i) => ({ ...m, width: sane(sizes[i].width), height: sane(sizes[i].height) }));
  // From monitor 0 out: each one goes beside the monitor it touched first
  const placed = new Set([0]);
  const queue = [0];
  while (queue.length) {
    const p = queue.shift()!;
    monitors.forEach((m, c) => {
      if (placed.has(c)) return;
      const side = sideOf(m, monitors[p]);
      if (!side) return;
      const o = monitors[p];
      const [P, C] = [next[p], next[c]];
      // The offset along the shared edge: lined up stays lined up, else kept in proportion
      const offset = (start: number, size: number, oStart: number, oSize: number, nSize: number, cSize: number) =>
        start === oStart ? 0 : start + size === oStart + oSize ? nSize - cSize : ((start - oStart) * nSize) / oSize;
      if (side === "right" || side === "left") {
        C.x = side === "right" ? P.x + P.width : P.x - C.width;
        C.y = along(P.y + offset(m.y, m.height, o.y, o.height, P.height, C.height), C.height, P.y, P.height);
      } else {
        C.y = side === "below" ? P.y + P.height : P.y - C.height;
        C.x = along(P.x + offset(m.x, m.width, o.x, o.width, P.width, C.width), C.width, P.x, P.width);
      }
      placed.add(c);
      queue.push(c);
    });
  }
  // One that touched nothing, or that now covers another, goes to its nearest free place
  let out = normalize(next);
  out.forEach((_, i) => {
    if (i === 0) return;
    if (!placed.has(i) || out.some((o, k) => k !== i && overlaps(out[i], o))) {
      out = dropMonitor(out, i, out[i].x, out[i].y);
    }
  });
  return out;
}

/** A new monitor `width`×`height` at the right of the rightmost one, lined up with its top. */
export function addMonitor(monitors: MonitorSpec[], width = 1920, height = 1080): MonitorSpec[] {
  if (monitors.length >= MAX_MONITORS) return monitors;
  if (!monitors.length) return normalize([{ width, height, x: 0, y: 0, primary: true }]);
  const right = monitors.reduce((a, m) => (m.x + m.width > a.x + a.width ? m : a));
  return normalize([...monitors, { width, height, x: right.x + right.width, y: right.y, primary: false }]);
}

/** The arrangement without monitor `i` (never the last one); the rest close up. */
export function removeMonitor(monitors: MonitorSpec[], i: number): MonitorSpec[] {
  if (monitors.length < 2) return monitors;
  const rest = monitors.filter((_, k) => k !== i);
  return resize(rest, rest);
}

/** The new numbers after `removeMonitor(…, i)` of `n` monitors (`map[old]`; null: removed). */
export function removeMap(n: number, i: number): (number | null)[] {
  if (n < 2) return Array.from({ length: n }, (_, k) => k);
  return Array.from({ length: n }, (_, k) => (k === i ? null : k > i ? k - 1 : k));
}

/** The new numbers after `makePrimary(…, i)` of `n` monitors (`map[old]`). */
export function primaryMap(n: number, i: number): number[] {
  return Array.from({ length: n }, (_, k) => (k === i ? 0 : k < i ? k + 1 : k));
}

/** Per-monitor values (`values[k]` for monitor k) after the monitors were renumbered
 *  (`map[old]` is the new number; null: removed). */
export function renumber<T>(values: T[], map: (number | null)[]): T[] {
  const out: T[] = [];
  map.forEach((to, k) => {
    if (to !== null) out[to] = values[k];
  });
  return out;
}

/** The size monitor `m` gets when its pixel scale goes from `from` to `to`: its viewer window
 *  keeps its size in screen pixels, so the clone pixels in it change. */
export function rescale(m: { width: number; height: number }, from: number, to: number) {
  return { width: (m.width * from) / to, height: (m.height * from) / to };
}

/** Monitor `i` becomes monitor 0, the primary one; the arrangement stays. */
export function makePrimary(monitors: MonitorSpec[], i: number): MonitorSpec[] {
  if (i === 0) return monitors;
  return normalize([monitors[i], ...monitors.filter((_, k) => k !== i)]);
}
