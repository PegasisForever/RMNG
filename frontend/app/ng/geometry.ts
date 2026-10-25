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

type Box = { left: number; top: number; width: number; height: number };

export type KeysLayout = {
  /** One key unit plus the gap after it: a 1-unit key is `pitch - GAP` wide. */
  pitch: number;
  /** A key's height: KEY_ASPECT × a 1-unit key's width, or less where the rows do not fit. */
  keyHeight: number;
  /** The tool row: at the top of the area, under the screen. */
  tools: Box;
  /** The keys: right under the tool row; what is left of the area stays empty below them. */
  keys: Box;
  /** The trackpad: all the space under the tool row, down to where the keys end. */
  pad: Box;
};

/** Where things go in a keyboard area `width`×`height` with `bottom` kept for the home bar.
 *  The keys take the whole width, and are KEY_ASPECT times as tall as wide where the area has
 *  room for that; where it has not, they are as tall as the rows can be. */
export function keysLayout(width: number, height: number, bottom: number): KeysLayout {
  const room = height - PAD - TOOL_H - GAP - bottom;
  const pitch = (width - 2 * PAD + GAP) / ROW_UNITS;
  const keyHeight = Math.max(Math.min((pitch - GAP) * KEY_ASPECT, (room - (ROWS - 1) * GAP) / ROWS), 1);
  const w = ROW_UNITS * pitch - GAP;
  const h = ROWS * keyHeight + (ROWS - 1) * GAP;
  const left = PAD;
  const tools = { left: PAD, top: PAD, width: width - 2 * PAD, height: TOOL_H };
  const padTop = PAD + TOOL_H + GAP;
  const keysTop = padTop;
  return {
    pitch,
    keyHeight,
    tools,
    keys: { left, top: keysTop, width: w, height: h },
    pad: { left: PAD, top: padTop, width: width - 2 * PAD, height: height - bottom - padTop },
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
 *  middle; `dpr` device px per CSS px. */
export function screenFit(areaW: number, areaH: number, monW: number, monH: number, dpr: number): ScreenFit {
  const fit = Math.min((areaW * dpr) / monW, (areaH * dpr) / monH);
  const whole = Math.floor(fit);
  const scale = whole >= 1 && fit - whole < SNAP * whole ? whole : fit;
  const width = (monW * scale) / dpr;
  const height = (monH * scale) / dpr;
  return { scale, left: (areaW - width) / 2, top: (areaH - height) / 2, width, height };
}
