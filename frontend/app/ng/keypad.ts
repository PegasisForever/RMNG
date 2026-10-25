// The /ng keys are also a touchpad. Each touch on a key is a key or a pointer gesture, decided
// by what the finger does:
//
// - It lifts without moving: a tap of that key.
// - It stays down without moving for HOLD_MS: the key goes down, and up when the finger
//   lifts, so the clone's key repeat works (Backspace held deletes on).
// - It moves past SLOP_PX first: it types nothing. Alone, it moves the cursor; with other
//   fingers down that have not typed either, they scroll together.
//
// A modifier is told as it goes: down when the finger comes down, up when it lifts or when
// its touch turns into a pointer gesture (`./modifiers.ts` decides what that means). Fn is
// tapped, as any other key.
//
// Trackpad mode: once a touch moves the cursor or scrolls, the keys are a trackpad (`pad`
// on) until no finger has been on them for PAD_LINGER_MS. Meanwhile a touch types nothing: a
// move moves the cursor, two scroll, and a quick tap (under TAP_MS) clicks, as on a touchpad:
// the click waits TAP_WAIT_MS for a second touch. A second touch that moves drags (the left
// button down until it lifts); a second quick tap makes a double click. Each touch in that
// time starts the wait for the keys again.
//
// Fingers that roll from one key to the next (the next one down before the last is up) are
// still two taps, as neither moved. A touch that starts between keys has no key: it only ever
// moves the pointer. Pure: touches go in with their time, and what to do comes
// out through `out`; the hold wait runs on the `Timer` given, so tests can run it by hand.
import type { Key } from "./keyboard";

/** After the last finger of a pointer gesture lifts, the keys stay a trackpad this long. */
export const PAD_LINGER_MS = 1000;
/** In trackpad mode, a touch that lifts this soon without moving is a click. */
export const TAP_MS = 250;
/** In trackpad mode, a tap's click waits this long for a second touch, which drags or
 *  double-clicks. */
export const TAP_WAIT_MS = 250;
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
  /** A finger came down on the modifier `key`, or left it: it lifted, or its touch became a
   *  pointer gesture (`dragged`). */
  | { t: "mod"; key: Key; down: boolean; dragged?: boolean }
  /** One-finger motion since the last one, CSS px, and the time it took, ms. */
  | { t: "move"; dx: number; dy: number; ms: number }
  /** Motion of fingers that scroll together since the last one, CSS px (their mean). */
  | { t: "scroll"; dx: number; dy: number }
  /** The fingers that scrolled lifted. */
  | { t: "scroll_end" }
  /** The keys became a trackpad, or keys again. */
  | { t: "pad"; on: boolean }
  /** A tap in trackpad mode: a left click. */
  | { t: "click" }
  /** A drag in trackpad mode: the left button goes down, or up. */
  | { t: "drag"; pressed: boolean };

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
  /** It came down in trackpad mode: it never types, and its modifier was not told down. */
  pad: boolean;
  /** When it came down. */
  t0: number;
  /** It came down while a tap's click waited: a move drags, a tap double-clicks. */
  afterTap: boolean;
  /** It is dragging: the left button is down. */
  dragging: boolean;
};

export class Keypad {
  private touches = new Map<number, Touch>();
  /** Trackpad mode, and the cancel of the wait that ends it. */
  private pad = false;
  private cancelLinger = () => {};
  /** The cancel of a tap's click that waits for a second touch, or null. */
  private waitingClick: (() => void) | null = null;

  constructor(
    private out: (o: KeypadOut) => void,
    private timer: Timer,
  ) {}

  down(id: number, key: Key | null, x: number, y: number, t: number) {
    const pad = this.pad;
    const touch: Touch = {
      key,
      x,
      y,
      t,
      sx: x,
      sy: y,
      is: "new",
      cancelHold: () => {},
      pad,
      t0: t,
      afterTap: false,
      dragging: false,
    };
    if (pad) {
      this.cancelLinger();
      if (this.waitingClick) {
        this.waitingClick();
        this.waitingClick = null;
        // A second finger beside a lone one after a tap: no drag; the tap still clicks
        if (this.touches.size) this.out({ t: "click" });
        else touch.afterTap = true;
      }
    }
    else if (key?.mod) this.out({ t: "mod", key, down: true });
    else if (key && !key.fn) {
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
      if (p.key?.mod && !p.pad) this.out({ t: "mod", key: p.key, down: false, dragged: true });
      const others = [...this.touches.values()].filter(
        (o) => o !== p && (o.is === "new" || o.is === "point" || o.is === "scroll"),
      );
      if (others.length) {
        // Two (or more) fingers: they all scroll, none of them types
        for (const o of others) {
          o.cancelHold();
          if (o.is === "new" && o.key?.mod && !o.pad) {
            this.out({ t: "mod", key: o.key, down: false, dragged: true });
          }
          o.is = "scroll";
        }
        p.is = "scroll";
      } else p.is = "point";
      if (p.afterTap && p.is === "point") {
        p.dragging = true;
        this.out({ t: "drag", pressed: true });
      }
      if (!this.pad) {
        this.pad = true;
        this.out({ t: "pad", on: true });
      }
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

  up(id: number, t: number) {
    const p = this.touches.get(id);
    if (!p) return;
    this.touches.delete(id);
    p.cancelHold();
    if (p.dragging) this.out({ t: "drag", pressed: false });
    else if (p.afterTap && p.is === "new") {
      // The first tap's click, and this one's when it was a tap too: a double click
      this.out({ t: "click" });
      if (t - p.t0 < TAP_MS) this.out({ t: "click" });
    } else if (p.is === "new") {
      if (p.key?.mod && !p.pad) {
        // A modifier lifted while the keys were a trackpad was no tap: put it back
        this.out(this.pad ? { t: "mod", key: p.key, down: false, dragged: true } : { t: "mod", key: p.key, down: false });
      } else if (this.pad) {
        if (t - p.t0 < TAP_MS) {
          this.waitingClick = this.timer.after(TAP_WAIT_MS, () => {
            this.waitingClick = null;
            this.out({ t: "click" });
          });
        }
      } else if (p.key) this.out({ t: "tap", key: p.key });
    } else if (p.is === "hold" && p.key) this.out({ t: "hold", key: p.key, pressed: false });
    else if (p.is === "scroll" && ![...this.touches.values()].some((o) => o.is === "scroll")) {
      this.out({ t: "scroll_end" });
    }
    if (this.pad && this.touches.size === 0) this.linger();
  }

  /** The keys stay a trackpad PAD_LINGER_MS more, unless a finger comes down again. */
  private linger() {
    this.cancelLinger();
    this.cancelLinger = this.timer.after(PAD_LINGER_MS, () => {
      if (this.touches.size) return;
      this.pad = false;
      this.out({ t: "pad", on: false });
    });
  }

  /** The touch ended without the finger lifting (the system took it): it types nothing, and
   *  a key it holds goes up. */
  cancel(id: number) {
    const p = this.touches.get(id);
    if (!p) return;
    if (p.is === "new") {
      if (p.key?.mod && !p.pad) this.out({ t: "mod", key: p.key, down: false, dragged: true });
      p.is = "point";
    }
    this.up(id, Infinity);
  }
}
