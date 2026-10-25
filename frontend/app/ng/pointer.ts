// The /ng pointer rules: the red dot (a TrackPoint) and scroll by dragging the middle button.

/** A push on the red dot shorter than this (CSS px) does not move the cursor, so a finger
 *  that rests on it does not drift. */
export const DEAD_PX = 2;
/** Cursor speed in screen (device) px per second: GAIN × (push − DEAD_PX)^POWER, so a small
 *  push is slow and exact and a big one crosses the screen fast. */
const GAIN = 40;
const POWER = 1.6;
const MAX_SPEED = 6000;

/** The cursor speed (screen px per second) for a push of `dx`, `dy` CSS px from where the
 *  finger first touched the dot, in the push's direction. */
export function dotVelocity(dx: number, dy: number): { vx: number; vy: number } {
  const r = Math.hypot(dx, dy);
  if (r <= DEAD_PX) return { vx: 0, vy: 0 };
  const speed = Math.min(GAIN * (r - DEAD_PX) ** POWER, MAX_SPEED);
  return { vx: (dx / r) * speed, vy: (dy / r) * speed };
}

/** `v` held to the pixels of an axis `size` pixels long. */
export function clampTo(v: number, size: number): number {
  return Math.min(Math.max(v, 0), Math.max(size - 1, 0));
}

/** A drag on the middle button longer than this (CSS px) scrolls; a shorter touch clicks. */
export const SCROLL_START_PX = 6;
/** One wheel notch per this much drag (CSS px). */
export const NOTCH_PX = 14;

/** The wheel notches a middle-button drag has earned since the last call: `moved` is the drag
 *  since then (CSS px, down is positive) and `rem` the part of a notch left over, which this
 *  updates. Down scrolls down (positive), up scrolls up. */
export function scrollSteps(moved: number, rem: { y: number }): number {
  rem.y += moved / NOTCH_PX;
  const steps = Math.trunc(rem.y);
  rem.y -= steps;
  return steps;
}
