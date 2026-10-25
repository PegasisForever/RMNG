// Sizes on the /ng screen: the keyboard area (a tool row over the keys, at the bottom) and the
// clone's screen above it, letterboxed when the area is not the monitor's shape.
import { ROWS, ROW_UNITS } from "./keyboard";

/** Space between keys, and around the keyboard at the sides and the top, CSS px. */
export const GAP = 4;
const PAD = 4;
/** The tool row's height, CSS px. */
export const TOOL_H = 36;

export type KeysLayout = {
  /** One key unit plus the gap after it: a 1-unit key is `pitch - GAP` wide and tall. */
  pitch: number;
  /** The keys' box (CSS px, from the area's top left): as wide as the area allows, its keys
   *  square, and at the bottom of the area, above the home bar. */
  left: number;
  top: number;
  width: number;
  height: number;
};

/** The pitch the area's width allows. */
const widthPitch = (width: number) => (width - 2 * PAD + GAP) / ROW_UNITS;

/** Where the keys go in a keyboard area `width`×`height` with `bottom` kept for the home bar.
 *  Keys are square; an area too short for that makes them smaller (the keys then stand in
 *  the middle), and a taller one leaves the space above the tool row empty. */
export function keysLayout(width: number, height: number, bottom: number): KeysLayout {
  const room = height - PAD - TOOL_H - GAP - bottom;
  const pitch = Math.max(Math.min(widthPitch(width), (room + GAP) / ROWS), 1);
  const w = ROW_UNITS * pitch - GAP;
  const h = ROWS * pitch - GAP;
  return { pitch, left: (width - w) / 2, top: height - bottom - h, width: w, height: h };
}

/** The height a keyboard area needs for square keys across the whole width. */
export function naturalKeysHeight(width: number, bottom: number): number {
  return Math.ceil(PAD + TOOL_H + GAP + ROWS * widthPitch(width) - GAP + bottom);
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
 *  middle; `dpr` device px per CSS px. */
export function screenFit(areaW: number, areaH: number, monW: number, monH: number, dpr: number): ScreenFit {
  const fit = Math.min((areaW * dpr) / monW, (areaH * dpr) / monH);
  const whole = Math.floor(fit);
  const scale = whole >= 1 && fit - whole < SNAP * whole ? whole : fit;
  const width = (monW * scale) / dpr;
  const height = (monH * scale) / dpr;
  return { scale, left: (areaW - width) / 2, top: (areaH - height) / 2, width, height };
}
