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
// move moves the cursor from its first pixel (no SLOP_PX wait), two scroll, and a quick tap
// (under TAP_MS, and less than TAP_SLOP_PX of motion) clicks, as on a touchpad:
// the click waits TAP_WAIT_MS for a second touch. A second touch that moves drags (the left
// button down until it lifts); a second quick tap makes a double click. Each touch in that
// time starts the wait for the keys again. The mouse buttons show only in trackpad mode, and
// one held down keeps the mode on (`button`), so they do not go while in use.
//
// Three fingers that move together are a swipe, as on a laptop touchpad: one gesture each
// time they go SWIPE_PX one way (`swipe`; the container sends its keys). Two fingers that tap
// together (all down and up within TAP_MS, none moving TAP_SLOP_PX) are a right click, three a
// middle click.
//
// `pinch`: two fingers are a scroll or a pinch, whichever they show first: their distance
// changing by PINCH_PX makes a pinch (`zoom`, the change as a factor; the clone gets nothing),
// their middle moving by TWO_SLOP_PX a scroll. Without it, two fingers always scroll.
//
// `alwaysPad`: the keypad is a trackpad only, with no keys (the whole screen while the keyboard
// is not docked, `./FullDesk.tsx`): trackpad mode from the start, for good.
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
/** In trackpad mode a touch moves the cursor from the first pixel, and it is still a tap (a
 *  click) when it lifts within TAP_MS having gone less than this far (CSS px); a second touch
 *  after a tap drags once it has gone this far. */
export const TAP_SLOP_PX = 10;
/** A drag on Space types an arrow key for each this far (CSS px) sideways, and up or down. */
export const ARROW_STEP_X = 12;
export const ARROW_STEP_Y = 28;
/** Three fingers that go this far together (CSS px, their mean) make a swipe. */
export const SWIPE_PX = 30;
/** With `pinch`: two fingers whose distance changes this far (CSS px) pinch, and two whose
 *  middle moves this far first scroll. */
export const PINCH_PX = 16;
export const TWO_SLOP_PX = 8;

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
  /** Two fingers pinched: the view's zoom changes by `factor` (their distance now over before). */
  | { t: "zoom"; factor: number }
  /** Three fingers swiped this way together (once per swipe). */
  | { t: "swipe"; dir: "up" | "down" | "left" | "right" }
  /** A tap in trackpad mode: a left click; a two-finger tap a right one, three a middle one. */
  | { t: "click"; button?: "right" | "middle" }
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
  is: "new" | "hold" | "point" | "scroll" | "arrows" | "swipe" | "two" | "pinch";
  /** For the arrows: where the finger was at the last arrow key. */
  ax: number;
  ay: number;
  /** For a three-finger swipe: where the finger was when the swipe began. */
  gx: number;
  gy: number;
  cancelHold: () => void;
  /** It came down in trackpad mode: it never types, and its modifier was not told down. */
  pad: boolean;
  /** When it came down. */
  t0: number;
  /** It came down while a tap's click waited: a move drags, a tap double-clicks. */
  afterTap: boolean;
  /** It is dragging: the left button is down. */
  dragging: boolean;
  /** The farthest it has been from where it came down, CSS px. */
  far: number;
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
  /** The three-finger swipe now on has made its gesture: no second one until the fingers lift. */
  private swiped = false;
  /** The trackpad touches since no finger was down: the most at once, and whether one of them
   *  moved or stayed too long for a tap (`groupTap`). */
  private group = { most: 0, spoilt: false };
  private readonly always: boolean;
  private readonly pinch: boolean;
  /** Two fingers not yet a scroll or a pinch: their distance and middle when they began; and a
   *  pinch's distance at its last zoom. */
  private two = { d0: 0, cx: 0, cy: 0, last: 0 };

  constructor(
    private out: (o: KeypadOut) => void,
    private timer: Timer,
    opts: { alwaysPad?: boolean; pinch?: boolean } = {},
  ) {
    this.always = opts.alwaysPad === true;
    this.pinch = opts.pinch === true;
    this.pad = this.always;
  }

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
      far: 0,
      ax: x,
      ay: y,
      gx: x,
      gy: y,
    };
    if (this.touches.size === 0) this.group = { most: 0, spoilt: false };
    this.group.most = Math.max(this.group.most, this.touches.size + 1);
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
      // Fn and dictation send nothing, and Space only types on the lift: none is held
      else if (key.code !== 0 && key.code !== SPACE) {
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
    const d = Math.hypot(x - p.sx, y - p.sy);
    p.far = Math.max(p.far, d);
    if (p.is === "new") {
      // A key waits to see a tap; the trackpad moves the cursor at once
      if (p.pad ? d === 0 : d < SLOP_PX) return;
      p.cancelHold();
      if (!p.pad && p.key?.code === SPACE) {
        p.is = "arrows";
        return this.arrows(p, x, y);
      }
      if (p.key?.mod && !p.pad) this.out({ t: "mod", key: p.key, down: false, dragged: true });
      const others = [...this.touches.values()].filter(
        (o) =>
          o !== p &&
          (o.is === "new" || o.is === "point" || o.is === "scroll" || o.is === "swipe" || o.is === "two" || o.is === "pinch"),
      );
      // Three (or more) fingers swipe; two scroll (or, with `pinch`, wait to see); none types
      const next = others.length >= 2 ? "swipe" : others.length ? (this.pinch ? "two" : "scroll") : "point";
      if (next === "swipe" && others.some((o) => o.is === "scroll")) this.out({ t: "scroll_end" });
      for (const o of next === "point" ? [] : others) {
        o.cancelHold();
        if (o.is === "new" && o.key?.mod && !o.pad) {
          this.out({ t: "mod", key: o.key, down: false, dragged: true });
        }
        if (o.dragging) {
          o.dragging = false;
          this.out({ t: "drag", pressed: false });
        }
        if (next === "swipe" && o.is !== "swipe") Object.assign(o, { gx: o.x, gy: o.y });
        o.is = next;
      }
      p.is = next;
      if (next === "swipe") Object.assign(p, { gx: x, gy: y });
      if (next === "two") {
        const o = others[0];
        this.two = { d0: Math.hypot(x - o.x, y - o.y), cx: (x + o.x) / 2, cy: (y + o.y) / 2, last: 0 };
      }
      if (!this.pad) {
        this.pad = true;
        this.out({ t: "pad", on: true });
      }
    }
    if (p.is === "swipe") return this.swipe(p, x, y);
    if (p.is === "two" || p.is === "pinch") {
      Object.assign(p, { x, y, t });
      return this.twoFingers();
    }
    if (p.afterTap && p.is === "point" && !p.dragging && p.far >= TAP_SLOP_PX) {
      p.dragging = true;
      this.out({ t: "drag", pressed: true });
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
    const quick = t - p.t0 < TAP_MS && p.far < TAP_SLOP_PX;
    if (!quick || !p.pad) this.group.spoilt = true;
    // Two or three fingers down together: their tap is one right or middle click, when all are up
    const many = p.pad && this.group.most >= 2 && (p.is === "new" || p.is === "point" || p.is === "two");
    if (p.dragging) this.out({ t: "drag", pressed: false });
    else if (many) {
      if (this.touches.size === 0 && !this.group.spoilt && this.group.most <= 3) {
        this.out({ t: "click", button: this.group.most === 2 ? "right" : "middle" });
      }
    }
    else if (p.afterTap && (p.is === "new" || p.is === "point" || p.is === "hold")) {
      // The first tap's click, and this one's when it was a tap too: a double click
      this.out({ t: "click" });
      if (quick) this.out({ t: "click" });
    } else if (p.is === "new" || (p.pad && p.is === "point")) {
      if (p.key?.mod && !p.pad) {
        // A modifier lifted while the keys were a trackpad was no tap: put it back
        this.out(this.pad ? { t: "mod", key: p.key, down: false, dragged: true } : { t: "mod", key: p.key, down: false });
      } else if (this.pad) {
        if (quick) {
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
    if (![...this.touches.values()].some((o) => o.is === "swipe")) this.swiped = false;
    if (this.pad && this.touches.size === 0 && this.buttons === 0) this.linger();
  }

  /** Two fingers with `pinch`: until they show which, a scroll or a pinch; in a pinch, the zoom
   *  for each change of their distance. */
  private twoFingers() {
    const f = [...this.touches.values()].filter((o) => o.is === "two" || o.is === "pinch");
    if (f.length !== 2) return;
    const d = Math.hypot(f[0].x - f[1].x, f[0].y - f[1].y);
    if (f[0].is === "pinch") {
      if (this.two.last > 0 && d > 0) this.out({ t: "zoom", factor: d / this.two.last });
      this.two.last = d;
      return;
    }
    const moved = Math.hypot((f[0].x + f[1].x) / 2 - this.two.cx, (f[0].y + f[1].y) / 2 - this.two.cy);
    if (Math.abs(d - this.two.d0) >= PINCH_PX) {
      for (const o of f) o.is = "pinch";
      if (this.two.d0 > 0) this.out({ t: "zoom", factor: d / this.two.d0 });
      this.two.last = d;
    } else if (moved >= TWO_SLOP_PX) {
      for (const o of f) o.is = "scroll";
    }
  }

  /** A three-finger swipe: once the fingers' mean has gone SWIPE_PX one way, that way's
   *  gesture, and no other until they lift. */
  private swipe(p: Touch, x: number, y: number) {
    Object.assign(p, { x, y });
    if (this.swiped) return;
    const fingers = [...this.touches.values()].filter((o) => o.is === "swipe");
    const mx = fingers.reduce((a, o) => a + o.x - o.gx, 0) / fingers.length;
    const my = fingers.reduce((a, o) => a + o.y - o.gy, 0) / fingers.length;
    if (Math.max(Math.abs(mx), Math.abs(my)) < SWIPE_PX) return;
    this.swiped = true;
    const dir = Math.abs(mx) > Math.abs(my) ? (mx > 0 ? "right" : "left") : my > 0 ? "down" : "up";
    this.out({ t: "swipe", dir });
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
    if (this.always) return;
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
