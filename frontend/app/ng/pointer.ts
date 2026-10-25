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

/** Scroll distance (the clone's surface px) per CSS px of two-finger motion. The content
 *  follows the fingers, as Windows does by default: fingers up scroll down. */
export const SCROLL_GAIN = -2;

/** `v` held to the pixels of an axis `size` pixels long. */
export function clampTo(v: number, size: number): number {
  return Math.min(Math.max(v, 0), Math.max(size - 1, 0));
}
