// The on-screen keyboard's modifier keys (Shift, Ctrl, Alt, Win), on the page's side:
//
// - Tapped, a modifier goes down for the next key: Ctrl, then C, is Ctrl+C, and Ctrl goes up
//   after the C (also after a mouse click). Tapped again before that, it goes up.
// - Held (a finger stays on it while other keys are typed), it stays down for all of them and
//   goes up when that finger lifts: Ctrl held, then A, then D.
// - A lockable one (Shift) tapped twice quickly locks: it stays down through any keys until
//   the next tap.
// - A touch on it that turned into a pointer gesture leaves it as it was before.
//
// Pure: what to send comes out through `send` (evdev code, pressed), and `states` is what the
// keys show.

/** Two taps on a lockable modifier this close together (from the first tap's lift to the
 *  second's touch) lock it. */
export const DOUBLE_TAP_MS = 350;

export type ModState = "held" | "latched" | "locked";

type Mod = {
  is: ModState;
  /** While held: what it was before the finger came down, whether a key was typed while held,
   *  and when the finger came down. */
  before: "off" | "latched" | "locked";
  used: boolean;
  downAt: number;
};

export class Modifiers {
  private mods = new Map<number, Mod>();
  /** When each modifier's last tap latched it. */
  private latchedAt = new Map<number, number>();

  constructor(
    private send: (code: number, pressed: boolean) => void,
    private lockable: (code: number) => boolean,
  ) {}

  /** A finger came down on the modifier `code`. */
  down(code: number, t: number) {
    const m = this.mods.get(code);
    if (!m) this.send(code, true);
    const before = !m ? "off" : m.is === "held" ? m.before : m.is;
    this.mods.set(code, { is: "held", before, used: false, downAt: t });
  }

  /** The finger on `code` lifted; `dragged` when its touch was a pointer gesture. */
  up(code: number, dragged: boolean, t: number) {
    const m = this.mods.get(code);
    if (!m || m.is !== "held") return;
    if (dragged) {
      if (m.before === "off") this.release(code);
      else m.is = m.before;
    } else if (m.used) {
      if (m.before === "locked") m.is = "locked";
      else this.release(code);
    } else if (m.before === "off") {
      m.is = "latched";
      this.latchedAt.set(code, t);
    } else if (
      m.before === "latched" &&
      this.lockable(code) &&
      m.downAt - (this.latchedAt.get(code) ?? -Infinity) < DOUBLE_TAP_MS
    ) {
      m.is = "locked";
    } else this.release(code);
  }

  /** A key that is no modifier (or a mouse button) goes down: the modifiers held count as used. */
  keyDown() {
    for (const m of this.mods.values()) if (m.is === "held") m.used = true;
  }

  /** That key (or button) is up again: the tapped modifiers have done their one key. */
  keyDone() {
    for (const [code, m] of this.mods) if (m.is === "latched") this.release(code);
  }

  states(): ReadonlyMap<number, ModState> {
    return new Map([...this.mods].map(([code, m]) => [code, m.is]));
  }

  private release(code: number) {
    this.mods.delete(code);
    this.send(code, false);
  }
}
