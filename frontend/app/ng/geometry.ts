// Sizes on the /ng screen: the keyboard area (a tool row over the keys, at the bottom) and the
// clone's screen above it, letterboxed when the area is not the monitor's shape.
import { ROWS, ROW_UNITS } from "./keyboard";

/** Space between keys, and around the keyboard at the sides and the top, CSS px. */
export const GAP = 4;
const PAD = 4;
/** The tool row's height, CSS px. */
export const TOOL_H = 36;
/** A key is at most this many times as tall as a 1-unit key is wide: square. */
export const KEY_ASPECT = 1;
/** The mouse buttons under the keys keep at least this height (with the home bar's, if
 *  that is more): the least touch target Apple gives. */
export const MIN_BUTTONS_H = 44;
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
  /** The tool row: at the top of the area, under the screen. */
  tools: Box;
  /** The keys: right under the tool row; what is left of the area stays empty below them. */
  keys: Box;
  /** The mouse buttons: all the space under the keys, down to the screen's bottom edge. */
  buttons: Box;
  /** An open tab's page, in the keys' and the mouse buttons' place: under the tool row, down
   *  to the home bar. */
  page: Box;
};

/** Where things go in a keyboard area `width`×`height` with `bottom` kept for the home bar;
 *  with `bare`, the keys only (no tool row, no mouse buttons).
 *  The keys take the whole width and are square where the area has room for that. Where it
 *  has not, the symbol and number rows get shorter and the four rows under them stay square;
 *  only when the two are down to MIN_TOP_ROW do those get shorter too. */
export function keysLayout(width: number, height: number, bottom: number, bare = false): KeysLayout {
  // `bare`: the keys alone (the floating keyboard of the other orientations), down to `bottom`
  const room = bare
    ? height - PAD - PAD - bottom
    : height - PAD - TOOL_H - GAP - GAP - Math.max(bottom, MIN_BUTTONS_H);
  const pitch = (width - 2 * PAD + GAP) / ROW_UNITS;
  const full = (pitch - GAP) * KEY_ASPECT;
  const rowsRoom = room - (ROWS - 1) * GAP;
  const low = ROWS - TOP_ROWS;
  const top = Math.min(full, Math.max((rowsRoom - low * full) / TOP_ROWS, MIN_TOP_ROW));
  const lower = Math.max(Math.min(full, (rowsRoom - TOP_ROWS * top) / low), 1);
  const rowHeights = Array.from({ length: ROWS }, (_, i) => (i < TOP_ROWS ? top : lower));
  const w = ROW_UNITS * pitch - GAP;
  const h = rowHeights.reduce((a, b) => a + b, 0) + (ROWS - 1) * GAP;
  const tools = { left: PAD, top: PAD, width: width - 2 * PAD, height: bare ? 0 : TOOL_H };
  const keysTop = bare ? PAD : PAD + TOOL_H + GAP;
  const buttonsTop = keysTop + h + GAP;
  return {
    pitch,
    rowHeights,
    tools,
    keys: { left: PAD, top: keysTop, width: w, height: h },
    buttons: { left: PAD, top: buttonsTop, width: width - 2 * PAD, height: height - buttonsTop },
    page: { left: PAD, top: keysTop, width: width - 2 * PAD, height: height - bottom - keysTop },
  };
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

/** Device px per monitor px on the phone, in every orientation: the iPhone Duo layout is shown
 *  at 2× upright, and the other orientations keep that, so text is the same size on the clone
 *  whichever way the phone is held. */
export const PHONE_SCALE = 2;

/** The monitor for a screen of `width`×`height` CSS px at `dpr`: PHONE_SCALE device px per
 *  monitor px, in even sizes (the encoder takes even sizes only). */
export function phoneMonitorSize(width: number, height: number, dpr: number): { width: number; height: number } {
  const even = (css: number) => Math.floor((css * dpr) / PHONE_SCALE / 2) * 2;
  return { width: even(width), height: even(height) };
}

/** How far to move the clone's screen up (CSS px) so its cursor, `cursorY` from the page top,
 *  stays `margin` above the top of a keyboard over the screen, `visible` px down the page. Never
 *  more than it takes to bring the screen's bottom edge, `bottom`, to the keyboard's top: the
 *  screen moves only as far as the cursor needs, and stays put while the cursor is high. */
export function cursorShift(cursorY: number, visible: number, bottom: number, margin: number): number {
  return Math.max(0, Math.min(cursorY + margin - visible, bottom - visible));
}
