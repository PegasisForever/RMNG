// The on-screen keyboard typing into a headless clone's terminal (`./TerminalScreen.tsx`): its keys send
// evdev codes (`./keyboard.ts`), as to a desktop, and this turns them into the bytes a terminal's
// keys make (xterm's: a US keyboard, Ctrl and Alt as a terminal reads them). It keeps which
// modifiers are down, from the same presses, and Caps Lock, as the keyboard says it (`locks`);
// a click takes the modifiers too.
import { CODE_TO_EVDEV } from "~/viewer/keycodes";

const ESC = "\x1b";

/** What a key types, and with Shift. */
const CHARS: Record<string, [string, string]> = {
  Backquote: ["`", "~"],
  Minus: ["-", "_"],
  Equal: ["=", "+"],
  BracketLeft: ["[", "{"],
  BracketRight: ["]", "}"],
  Backslash: ["\\", "|"],
  Semicolon: [";", ":"],
  Quote: ["'", '"'],
  Comma: [",", "<"],
  Period: [".", ">"],
  Slash: ["/", "?"],
  Space: [" ", " "],
};
[..."1234567890"].forEach((d, i) => (CHARS[`Digit${d}`] = [d, ")!@#$%^&*("[(i + 1) % 10]]));
for (const c of "abcdefghijklmnopqrstuvwxyz") CHARS[`Key${c.toUpperCase()}`] = [c, c.toUpperCase()];

/** Keys sent as `CSI 1 ; mods X` with a modifier, `CSI X` (or `SS3 X` in application mode, for
 *  the cursor keys) without. */
const LETTER_KEYS: Record<string, string> = {
  ArrowUp: "A",
  ArrowDown: "B",
  ArrowRight: "C",
  ArrowLeft: "D",
  Home: "H",
  End: "F",
};
/** F1 to F4: `SS3 X`, or `CSI 1 ; mods X` with a modifier. */
const SS3_KEYS: Record<string, string> = { F1: "P", F2: "Q", F3: "R", F4: "S" };
/** Keys sent as `CSI n ~`, or `CSI n ; mods ~` with a modifier. */
const TILDE_KEYS: Record<string, number> = {
  Insert: 2,
  Delete: 3,
  PageUp: 5,
  PageDown: 6,
  F5: 15,
  F6: 17,
  F7: 18,
  F8: 19,
  F9: 20,
  F10: 21,
  F11: 23,
  F12: 24,
};

const BY_CODE = new Map(Object.entries(CODE_TO_EVDEV).map(([id, code]) => [code, id]));

const SHIFTS = new Set([CODE_TO_EVDEV.ShiftLeft, CODE_TO_EVDEV.ShiftRight]);
const CTRLS = new Set([CODE_TO_EVDEV.ControlLeft, CODE_TO_EVDEV.ControlRight]);
const ALTS = new Set([CODE_TO_EVDEV.AltLeft, CODE_TO_EVDEV.AltRight]);
const METAS = new Set([CODE_TO_EVDEV.MetaLeft, CODE_TO_EVDEV.MetaRight]);

export type TermMods = { shift: boolean; ctrl: boolean; alt: boolean };

/** Ctrl with a key that is no letter: the control code, as xterm makes it. */
const CONTROLS: Record<string, number> = {
  " ": 0,
  "2": 0,
  "@": 0,
  "[": 27,
  "3": 27,
  "\\": 28,
  "4": 28,
  "]": 29,
  "5": 29,
  "6": 30,
  "^": 30,
  "-": 31,
  "_": 31,
  "/": 31,
  "7": 31,
  "8": 127,
  "?": 127,
};

/** Ctrl with a key: the control code it makes, or null when it makes none. */
function control(c: string): string | null {
  if (/^[a-z]$/i.test(c)) return String.fromCharCode(c.toLowerCase().charCodeAt(0) - 96);
  const k = CONTROLS[c];
  return k === undefined ? null : String.fromCharCode(k);
}

/** The bytes the key `id` (a DOM code) types with `mods` and Caps Lock; null: none (a modifier,
 *  the Win key, Print Screen). `appCursor`: the terminal asked for the cursor keys'
 *  application mode. */
export function termBytes(id: string, mods: TermMods, caps: boolean, appCursor: boolean): string | null {
  const m = 1 + (mods.shift ? 1 : 0) + (mods.alt ? 2 : 0) + (mods.ctrl ? 4 : 0);
  if (id in LETTER_KEYS) {
    if (m > 1) return `${ESC}[1;${m}${LETTER_KEYS[id]}`;
    return `${ESC}${appCursor && id.startsWith("Arrow") ? "O" : "["}${LETTER_KEYS[id]}`;
  }
  if (id in SS3_KEYS) return m > 1 ? `${ESC}[1;${m}${SS3_KEYS[id]}` : `${ESC}O${SS3_KEYS[id]}`;
  if (id in TILDE_KEYS) return m > 1 ? `${ESC}[${TILDE_KEYS[id]};${m}~` : `${ESC}[${TILDE_KEYS[id]}~`;

  const alt = (s: string) => (mods.alt ? ESC + s : s);
  switch (id) {
    case "Enter":
      return alt("\r");
    case "Backspace":
      return alt(mods.ctrl ? "\x08" : "\x7f");
    case "Tab":
      return mods.shift ? `${ESC}[Z` : alt("\t");
    case "Escape":
      return alt(ESC);
  }
  const chars = CHARS[id];
  if (!chars) return null;
  const letter = id.startsWith("Key");
  const c = chars[mods.shift !== (letter && caps) ? 1 : 0];
  if (mods.ctrl) {
    const k = control(c);
    return k === null ? null : alt(k);
  }
  return alt(c);
}

/** The keys' presses, as the clone would get them, made into a terminal's bytes. */
export class TermKeys {
  private held = new Set<number>();
  private caps = false;

  /** A key `code` (evdev) went down or up: what it types, or null. */
  key(code: number, pressed: boolean, appCursor: boolean): string | null {
    const isMod = SHIFTS.has(code) || CTRLS.has(code) || ALTS.has(code) || METAS.has(code);
    if (isMod) {
      if (pressed) this.held.add(code);
      else this.held.delete(code);
      return null;
    }
    if (!pressed) return null;
    const id = BY_CODE.get(code);
    if (!id) return null;
    const m = this.mods();
    return termBytes(id, { shift: m.shiftKey, ctrl: m.ctrlKey, alt: m.altKey }, this.caps, appCursor);
  }

  /** The keyboard's Caps Lock is on or off. */
  locks(caps: boolean) {
    this.caps = caps;
  }

  /** The modifiers down, as a mouse event says them (a Shift-click selects in a terminal whose
   *  app takes the mouse). */
  mods() {
    const has = (set: Set<number>) => [...set].some((c) => this.held.has(c));
    return { shiftKey: has(SHIFTS), ctrlKey: has(CTRLS), altKey: has(ALTS) };
  }
}
