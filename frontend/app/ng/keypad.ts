// The /ng keys are also a touchpad. Each touch on a key is a key or a pointer gesture, decided
// by what the finger does:
//
// - It lifts without moving: a tap of that key.
// - It stays down without moving for HOLD_MS: the key goes down, and up when the finger
//   lifts, so the clone's key repeat works (Backspace held deletes on).
// - It moves past SLOP_PX first: it types nothing. Alone, it moves the cursor; with other
//   fingers down that have not typed either, they scroll together.
//
// Fingers that roll from one key to the next (the next one down before the last is up) are
// still two taps, as neither moved. A touch that starts between keys has no key: it only ever
// moves the pointer. Pure: touches go in with their time, and what to do comes
// out through `out`; the hold wait runs on the `Timer` given, so tests can run it by hand.
import type { Key } from "./keyboard";

/** A finger that stays on a key this long without moving holds it down. */
export const HOLD_MS = 250;
/** A finger that moves this far (CSS px) before it lifts or holds is a pointer gesture. Larger
 *  than a fingertip rolls on a tap. */
export const SLOP_PX = 10;

export type KeypadOut =
  /** A finger tapped `key` (it lifted without moving or holding). */
  | { t: "tap"; key: Key }
  /** A finger holds `key` down (`pressed`), or lifted after holding it. */
  | { t: "hold"; key: Key; pressed: boolean }
  /** One-finger motion since the last one, CSS px, and the time it took, ms. */
  | { t: "move"; dx: number; dy: number; ms: number }
  /** Motion of fingers that scroll together since the last one, CSS px (their mean). */
  | { t: "scroll"; dx: number; dy: number }
  /** The fingers that scrolled lifted. */
  | { t: "scroll_end" };

export interface Timer {
  /** Run `fn` in `ms`; returns the cancel. */
  after(ms: number, fn: () => void): () => void;
}

type Touch = {
  key: Key | null;
  x: number;
  y: number;
  t: number;
  sx: number;
  sy: number;
  /** Not decided yet, holding its key, moving the cursor, or scrolling. */
  is: "new" | "hold" | "point" | "scroll";
  cancelHold: () => void;
};

export class Keypad {
  private touches = new Map<number, Touch>();

  constructor(
    private out: (o: KeypadOut) => void,
    private timer: Timer,
  ) {}

  down(id: number, key: Key | null, x: number, y: number, t: number) {
    const touch: Touch = { key, x, y, t, sx: x, sy: y, is: "new", cancelHold: () => {} };
    // A modifier or Fn is only ever tapped: it holds itself down (see `Key.mod`)
    if (key && !key.mod && !key.fn) {
      touch.cancelHold = this.timer.after(HOLD_MS, () => {
        if (touch.is !== "new") return;
        touch.is = "hold";
        this.out({ t: "hold", key, pressed: true });
      });
    }
    // A finger that joins a moving one scrolls with it once it moves too
    this.touches.set(id, touch);
  }

  move(id: number, x: number, y: number, t: number) {
    const p = this.touches.get(id);
    if (!p || p.is === "hold") return;
    if (p.is === "new") {
      if (Math.hypot(x - p.sx, y - p.sy) < SLOP_PX) return;
      p.cancelHold();
      const others = [...this.touches.values()].filter(
        (o) => o !== p && (o.is === "new" || o.is === "point" || o.is === "scroll"),
      );
      if (others.length) {
        // Two (or more) fingers: they all scroll, none of them types
        for (const o of others) {
          o.cancelHold();
          o.is = "scroll";
        }
        p.is = "scroll";
      } else p.is = "point";
    }
    const dx = x - p.x;
    const dy = y - p.y;
    const ms = t - p.t;
    Object.assign(p, { x, y, t });
    if (p.is === "point") this.out({ t: "move", dx, dy, ms });
    else {
      const n = [...this.touches.values()].filter((o) => o.is === "scroll").length;
      this.out({ t: "scroll", dx: dx / n, dy: dy / n });
    }
  }

  up(id: number) {
    const p = this.touches.get(id);
    if (!p) return;
    this.touches.delete(id);
    p.cancelHold();
    if (p.is === "new") {
      if (p.key) this.out({ t: "tap", key: p.key });
    } else if (p.is === "hold" && p.key) this.out({ t: "hold", key: p.key, pressed: false });
    else if (p.is === "scroll" && ![...this.touches.values()].some((o) => o.is === "scroll")) {
      this.out({ t: "scroll_end" });
    }
  }

  /** The touch ended without the finger lifting (the system took it): it types nothing, and
   *  a key it holds goes up. */
  cancel(id: number) {
    const p = this.touches.get(id);
    if (!p) return;
    if (p.is === "new") p.is = "point";
    this.up(id);
  }
}
