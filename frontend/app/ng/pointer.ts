// The /ng pointer rules: how far the cursor moves for a finger on the trackpad, and how far
// two fingers scroll.

/** The cursor moves this many screen px per px of finger motion when the finger is slow, and
 *  up to MAX_GAIN when it is fast (ACCEL more per px/ms of speed), as on a touchpad. */
const BASE_GAIN = 1;
const ACCEL = 1.5;
const MAX_GAIN = 4;

/** Screen px the cursor moves for a finger motion of `d` CSS px that took `ms`. */
export function pointerGain(d: number, ms: number): number {
  const speed = Math.abs(d) / Math.max(ms, 1);
  return Math.min(BASE_GAIN + ACCEL * speed, MAX_GAIN);
}

/** Scroll distance (the clone's surface px) per CSS px of two-finger motion at a layout scroll
 *  speed of 100%. The content follows the fingers, as Windows does by default: fingers up
 *  scroll down. The layout's speed scales it (`scrollGain`); the iPhone Duo layout's is 15%
 *  (`wire::iphone_duo_layout`), as this base was much too fast on the Duo. It goes out as
 *  touchpad scroll (finger source, see `~/ng/Desk.tsx`), not as wheel notches. */
export const SCROLL_BASE = -2;

/** The iPhone Duo layout's speed, until the server's first word on it. */
export const DUO_SCROLL_SPEED = 15;

/** Scroll distance per CSS px for a layout scroll speed of `speed` percent (negative: the other
 *  way). */
export const scrollGain = (speed: number) => (SCROLL_BASE * speed) / 100;

/** `v` held to the pixels of an axis `size` pixels long. */
export function clampTo(v: number, size: number): number {
  return Math.min(Math.max(v, 0), Math.max(size - 1, 0));
}
