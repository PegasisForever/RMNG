// Sizes on the /ng screen: the keyboard (at the bottom, over the clone's screen or under it) and
// the clone's screen, letterboxed when its area is not the monitor's shape.
import { ROWS, ROW_UNITS } from "./keyboard";

/** Space between keys, and around the keyboard at the sides and the top, CSS px. */
export const GAP = 4;
const PAD = 4;
/** The tool row's height (over the pages), CSS px. */
export const TOOL_H = 36;
/** A key is at most this many times as tall as a 1-unit key is wide: square. */
export const KEY_ASPECT = 1;
/** The two top rows (symbols, numbers) give way first where the keys do not fit; they keep at
 *  least this height before the other rows give way too. */
const MIN_TOP_ROW = 24;
/** How many rows, from the top, give way first. */
const TOP_ROWS = 2;

type Box = { left: number; top: number; width: number; height: number };

export type KeysLayout = {
  /** One key unit plus the gap after it: a 1-unit key is `pitch - GAP` wide. */
  pitch: number;
  /** Each row's height, top first: KEY_ASPECT × a 1-unit key's width, or less where the
   *  rows do not fit, the two top rows first. */
  rowHeights: number[];
  /** The keys: at the top of the area; what is left of it stays empty below them. */
  keys: Box;
};

/** Where the keys go in a keyboard area `width`×`height` with `bottom` kept for the home bar.
 *  The keys take the whole width and are square where the area has room for that. Where it
 *  has not, the symbol and number rows get shorter and the four rows under them stay square;
 *  only when the two are down to MIN_TOP_ROW do those get shorter too. */
export function keysLayout(width: number, height: number, bottom: number): KeysLayout {
  const room = height - PAD - PAD - bottom;
  const pitch = (width - 2 * PAD + GAP) / ROW_UNITS;
  const full = (pitch - GAP) * KEY_ASPECT;
  const rowsRoom = room - (ROWS - 1) * GAP;
  const low = ROWS - TOP_ROWS;
  const top = Math.min(full, Math.max((rowsRoom - low * full) / TOP_ROWS, MIN_TOP_ROW));
  const lower = Math.max(Math.min(full, (rowsRoom - TOP_ROWS * top) / low), 1);
  const rowHeights = Array.from({ length: ROWS }, (_, i) => (i < TOP_ROWS ? top : lower));
  const w = ROW_UNITS * pitch - GAP;
  const h = rowHeights.reduce((a, b) => a + b, 0) + (ROWS - 1) * GAP;
  return { pitch, rowHeights, keys: { left: PAD, top: PAD, width: w, height: h } };
}

export type ScreenFit = {
  /** Device pixels per monitor pixel. */
  scale: number;
  /** The video's box in the screen area, CSS px. */
  left: number;
  top: number;
  width: number;
  height: number;
};

/** A scale less than this part above a whole number becomes that number, so each monitor
 *  pixel stays a square of whole screen pixels; the video is at most this much smaller. */
const SNAP = 0.02;

/** The monitor (`monW`×`monH` px) as large as fits in an area `areaW`×`areaH` CSS px, in the
 *  middle; `dpr` device px per CSS px. Never more than `maxScale` device px per monitor px: a
 *  monitor smaller than the area then stays at that scale, with black around it. */
export function screenFit(
  areaW: number,
  areaH: number,
  monW: number,
  monH: number,
  dpr: number,
  maxScale = Infinity,
): ScreenFit {
  const fit = Math.min((areaW * dpr) / monW, (areaH * dpr) / monH, maxScale);
  const whole = Math.floor(fit);
  const scale = whole >= 1 && fit - whole < SNAP * whole ? whole : fit;
  const width = (monW * scale) / dpr;
  const height = (monH * scale) / dpr;
  return { scale, left: (areaW - width) / 2, top: (areaH - height) / 2, width, height };
}

/** Device px per monitor px on the phone, unless the phone's own setting says
 *  (`./pixelRatio.ts`): 2, so text on the clone is as large as on the phone's own pages. */
export const PHONE_SCALE = 2;

/** The monitor for a screen of `width`×`height` CSS px at `dpr`: `scale` (PHONE_SCALE unless
 *  the phone's own setting says) device px per monitor px, in even sizes (the encoder takes
 *  even sizes only). */
export function phoneMonitorSize(
  width: number,
  height: number,
  dpr: number,
  scale = PHONE_SCALE,
): { width: number; height: number } {
  const even = (css: number) => Math.floor((css * dpr) / scale / 2) * 2;
  return { width: even(width), height: even(height) };
}

/** Where the clone's screen goes on one axis (its start, CSS px), so the cursor stays in view:
 *  `content` px long on a display `full` px long, of which the first `visible` px are not under
 *  the keyboard; the cursor `cursor` px from the screen's start; `prev` where it was. It moves
 *  as little as it can: not at all while the cursor stays `margin` inside the view.
 *
 *  A screen shorter than the display sits in its middle, and moves (up) only as far as needed to
 *  keep the cursor above the keyboard; a longer one (zoomed in) moves anywhere between its two
 *  ends lining up with the view's. */
export function followAxis(
  prev: number,
  cursor: number,
  content: number,
  visible: number,
  full: number,
  margin: number,
): number {
  const hi = content <= full ? (full - content) / 2 : 0;
  const lo = Math.min(hi, visible - content);
  let at = Math.min(hi, Math.max(lo, prev));
  const c = at + cursor;
  if (c < margin) at += margin - c;
  else if (c > visible - margin) at -= c - (visible - margin);
  return Math.min(hi, Math.max(lo, at));
}
