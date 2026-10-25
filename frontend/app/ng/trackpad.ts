// The /ng trackpad: touches on the key area, while it is in trackpad mode, become what a
// Windows precision touchpad does with them.
//
// - One finger moves the cursor.
// - A tap clicks (left). The click waits TAP_WAIT_MS for a second touch: a tap and then a
//   touch that stays down drags (left button down until it lifts); two taps double-click.
// - Two fingers move: scroll. Two fingers tap: right click.
// - Three fingers tap: middle click.
//
// Pure: touches go in with their time, and what to send comes out through `out`. The tap wait
// runs on the `Timer` given, so tests can run it by hand.

/** Evdev mouse buttons. */
export const BTN_LEFT = 0x110;
export const BTN_RIGHT = 0x111;
export const BTN_MIDDLE = 0x112;

/** A touch that lifts within this long, and moved less than SLOP_PX, is a tap. */
export const TAP_MS = 250;
/** Movement (CSS px) under this does not count, so a tap does not move the cursor. */
export const SLOP_PX = 4;
/** After a one-finger tap, a new touch within this long drags or double-clicks. */
export const TAP_WAIT_MS = 250;

export type PadOut =
  /** One-finger motion since the last one, CSS px, and the time it took, ms. */
  | { t: "move"; dx: number; dy: number; ms: number }
  | { t: "button"; button: number; pressed: boolean }
  /** Two-finger motion since the last one, CSS px (the fingers' mean). */
  | { t: "scroll"; dx: number; dy: number }
  /** The fingers that scrolled lifted. */
  | { t: "scroll_end" };

export interface Timer {
  /** Run `fn` in `ms`; returns the cancel. */
  after(ms: number, fn: () => void): () => void;
}

type Touch = { x: number; y: number; t: number; sx: number; sy: number };

export class Trackpad {
  private touches = new Map<number, Touch>();
  /** This gesture: when it began, the most fingers it had, whether it moved, scrolled. */
  private start = 0;
  private most = 0;
  private moved = false;
  private scrolled = false;
  /** The cancel of a tap's click that waits for a second touch. */
  private waiting: (() => void) | null = null;
  /** This gesture began within TAP_WAIT_MS of a tap; `dragging` once it moved (left down). */
  private afterTap = false;
  private dragging = false;

  constructor(
    private out: (o: PadOut) => void,
    private timer: Timer,
  ) {}

  down(id: number, x: number, y: number, t: number) {
    if (this.touches.size === 0) {
      this.start = t;
      this.most = 0;
      this.moved = this.scrolled = this.afterTap = false;
      if (this.waiting) {
        this.waiting();
        this.waiting = null;
        this.afterTap = true;
      }
    }
    this.touches.set(id, { x, y, t, sx: x, sy: y });
    this.most = Math.max(this.most, this.touches.size);
    // A second finger after a tap: no drag. The tap still clicks.
    if (this.most > 1 && this.afterTap && !this.dragging) {
      this.afterTap = false;
      this.click(BTN_LEFT);
    }
  }

  move(id: number, x: number, y: number, t: number) {
    const p = this.touches.get(id);
    if (!p) return;
    if (!this.moved && Math.hypot(x - p.sx, y - p.sy) < SLOP_PX) return;
    // The first move past the slop counts from where the finger was, so none of it is lost
    this.moved = true;
    const dx = x - p.x;
    const dy = y - p.y;
    const ms = t - p.t;
    Object.assign(p, { x, y, t });
    if (this.most === 1) {
      if (this.afterTap && !this.dragging) {
        this.dragging = true;
        this.out({ t: "button", button: BTN_LEFT, pressed: true });
      }
      this.out({ t: "move", dx, dy, ms });
    } else if (this.most === 2 && this.touches.size === 2) {
      this.scrolled = true;
      this.out({ t: "scroll", dx: dx / 2, dy: dy / 2 });
    }
  }

  up(id: number, t: number) {
    if (!this.touches.delete(id) || this.touches.size > 0) return;
    if (this.scrolled) this.out({ t: "scroll_end" });
    if (this.dragging) {
      this.dragging = false;
      this.out({ t: "button", button: BTN_LEFT, pressed: false });
      return;
    }
    const tap = !this.moved && t - this.start < TAP_MS;
    if (this.afterTap) {
      // The first tap's click, and this one's when it was a tap too: a double click
      this.click(BTN_LEFT);
      if (tap) this.click(BTN_LEFT);
      return;
    }
    if (!tap) return;
    if (this.most === 1) {
      this.waiting = this.timer.after(TAP_WAIT_MS, () => {
        this.waiting = null;
        this.click(BTN_LEFT);
      });
    } else this.click(this.most === 2 ? BTN_RIGHT : BTN_MIDDLE);
  }

  /** The trackpad goes away (back to the keys): let go of what is held, drop what waits. */
  cancel() {
    if (this.scrolled && this.touches.size) this.out({ t: "scroll_end" });
    if (this.dragging) this.out({ t: "button", button: BTN_LEFT, pressed: false });
    this.waiting?.();
    this.waiting = null;
    this.touches.clear();
    this.dragging = this.afterTap = this.scrolled = false;
  }

  private click(button: number) {
    this.out({ t: "button", button, pressed: true });
    this.out({ t: "button", button, pressed: false });
  }
}
