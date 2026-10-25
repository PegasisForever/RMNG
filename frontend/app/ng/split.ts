// Where the /ng page divides the screen (above the line) from the keyboard and controls
// (below it). `split` is the line's distance from the top of the screen, in CSS px.

/** The least height the screen area keeps, under the clock bar. */
export const MIN_SCREEN = 120;
/** The least height the keyboard area keeps, above the home bar. */
export const MIN_KEYS = 160;
/** Where the line starts: this part of the height under the clock bar is the screen. */
const DEFAULT_PART = 0.55;

export type Bounds = { height: number; top: number; bottom: number };

/** The line kept where both areas have their least height, in whole pixels. */
export function clampSplit(y: number, { height, top, bottom }: Bounds): number {
  const lo = top + MIN_SCREEN;
  const hi = height - bottom - MIN_KEYS;
  return Math.round(Math.min(Math.max(y, lo), Math.max(lo, hi)));
}

export function defaultSplit(b: Bounds): number {
  return clampSplit(b.top + (b.height - b.top) * DEFAULT_PART, b);
}
