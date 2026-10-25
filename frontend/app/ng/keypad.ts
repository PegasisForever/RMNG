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
// time starts the wait for the keys again. The mouse buttons show only in trackpad mode, and
// one held down keeps the mode on (`button`), so they do not go while in use.
//
// A drag that starts on Space is no pointer gesture: as on the iPhone keyboard, it moves the
// text cursor, one arrow key for each ARROW_STEP_X (sideways) or ARROW_STEP_Y (up and down)
// the finger goes, and the keys stay keys. Space types on the lift only, never held.
//
// Fingers that roll from one key to the next (the next one down before the last is up) are
// still two taps, as neither moved. A touch between keys is given the nearest key by the
// view. Pure: touches go in with their time, and what to do comes
// out through `out`; the hold wait runs on the `Timer` given, so tests can run it by hand.
import { ARROW_KEYS, type Key, SPACE } from "./keyboard";

/** After the last finger of a pointer gesture lifts, the keys stay a trackpad this long. */
export const PAD_LINGER_MS = 1000;
/** In trackpad mode, a touch that lifts this soon without moving is a click. */
export const TAP_MS = 250;
/** In trackpad mode, a tap's click waits this long for a second touch, which drags or
 *  double-clicks. */
export const TAP_WAIT_MS = 250;
/** A finger that stays on a key this long without moving holds it down. */
export const HOLD_MS = 250;
/** A finger that moves this far (CSS px) before it lifts or holds is a pointer gesture. Well
 *  over what a fingertip rolls on a tap (a key is about 54 px). */
export const SLOP_PX = 20;
/** A drag on Space types an arrow key for each this far (CSS px) sideways, and up or down. */
export const ARROW_STEP_X = 12;
export const ARROW_STEP_Y = 28;

export type KeypadOut =
  /** A finger came down on `key` while the keys are keys (the key's click sound plays). */
  | { t: "press"; key: Key }
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
  /** Not decided yet, holding its key, moving the cursor, scrolling, or (a drag on Space)
   *  typing arrow keys. */
  is: "new" | "hold" | "point" | "scroll" | "arrows";
  /** For the arrows: where the finger was at the last arrow key. */
  ax: number;
  ay: number;
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
  /** Mouse buttons held down now. */
  private buttons = 0;

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
      ax: x,
      ay: y,
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
    } else if (key) {
      this.out({ t: "press", key });
      if (key.mod) this.out({ t: "mod", key, down: true });
      else if (!key.fn && key.code !== SPACE) {
        touch.cancelHold = this.timer.after(HOLD_MS, () => {
          if (touch.is !== "new") return;
          touch.is = "hold";
          this.out({ t: "hold", key, pressed: true });
        });
      }
    }
    // A finger that joins a moving one scrolls with it once it moves too
    this.touches.set(id, touch);
  }

  move(id: number, x: number, y: number, t: number) {
    const p = this.touches.get(id);
    if (!p || p.is === "hold") return;
    if (p.is === "arrows") return this.arrows(p, x, y);
    if (p.is === "new") {
      if (Math.hypot(x - p.sx, y - p.sy) < SLOP_PX) return;
      p.cancelHold();
      if (!p.pad && p.key?.code === SPACE) {
        p.is = "arrows";
        return this.arrows(p, x, y);
      }
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
    if (this.pad && this.touches.size === 0 && this.buttons === 0) this.linger();
  }

  /** A mouse button went down or up. While one is held the keys stay a trackpad, and after the
   *  last one is up they stay one PAD_LINGER_MS more, as after a touch on them. */
  button(pressed: boolean) {
    this.buttons = Math.max(0, this.buttons + (pressed ? 1 : -1));
    if (!this.pad) return;
    if (pressed) this.cancelLinger();
    else if (this.touches.size === 0 && this.buttons === 0) this.linger();
  }

  /** The arrow keys for how far a drag on Space went since its last one. */
  private arrows(p: Touch, x: number, y: number) {
    const step = (d: number, size: number, less: Key, more: Key, at: "ax" | "ay") => {
      for (; d >= size; d -= size, p[at] += size) this.out({ t: "tap", key: more });
      for (; d <= -size; d += size, p[at] -= size) this.out({ t: "tap", key: less });
    };
    step(x - p.ax, ARROW_STEP_X, ARROW_KEYS.left, ARROW_KEYS.right, "ax");
    step(y - p.ay, ARROW_STEP_Y, ARROW_KEYS.up, ARROW_KEYS.down, "ay");
  }

  /** The keys stay a trackpad PAD_LINGER_MS more, unless a finger comes down again. */
  private linger() {
    this.cancelLinger();
    this.cancelLinger = this.timer.after(PAD_LINGER_MS, () => {
      if (this.touches.size || this.buttons) return;
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
