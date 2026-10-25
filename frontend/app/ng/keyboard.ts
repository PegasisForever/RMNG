// The /ng on-screen keyboard: a full Linux (US) keyboard in six rows of ROW_UNITS each. The
// symbols that sit beside the letters on a real keyboard have a row of their own (with Esc
// first), so the letters get the width. Every
// key sends its evdev code (`~/viewer/keycodes.ts`), so the clone's own keyboard layout
// applies, as with a real keyboard. The labels follow the latched Shift and Caps Lock.
import { CODE_TO_EVDEV } from "~/viewer/keycodes";

export type Key = {
  /** The DOM `KeyboardEvent.code` of the key; unique in a row set. */
  id: string;
  /** The evdev code it sends. */
  code: number;
  label: string;
  /** The label while Shift is down, when it is another one. */
  shift?: string;
  /** Width in key units (a letter key is 1). */
  units: number;
  /** A letter: Shift and Caps Lock both make it upper case. */
  letter?: boolean;
  /** A modifier: a tap holds it down, the next tap lets it go. */
  mod?: boolean;
  /** The Fn key: it switches the keys, and sends nothing itself (`code` is 0). */
  fn?: boolean;
  /** The dictation key: it starts and stops dictation (`./dictation.ts`), and sends nothing
   *  itself (`code` is 0). */
  dictate?: boolean;
  /** Drawn as this icon in place of its label. */
  icon?: "tab" | "enter" | "shift" | "win" | "mic";
};

/** The rows are this many units wide: ten letters and a Shift's worth more. The Tab row is
 *  half a unit shorter (a 1-unit Tab, as on a real keyboard), left aligned. */
export const ROW_UNITS = 11.5;
/** The keyboard's rows. */
export const ROWS = 6;

function key(id: string, label: string, units = 1, extra: Partial<Key> = {}): Key {
  const code = CODE_TO_EVDEV[id];
  if (code === undefined) throw new Error(`no evdev code for ${id}`);
  return { id, code, label, units, ...extra };
}

const shifted = (id: string, label: string, shift: string) => key(id, label, 1, { shift });
const letters = (s: string) => [...s].map((c) => key(`Key${c.toUpperCase()}`, c, 1, { letter: true }));
const mod = (id: string, label: string, units: number, extra: Partial<Key> = {}) =>
  key(id, label, units, { mod: true, ...extra });

const DIGITS = [...")!@#$%^&*("].map((shift, i) => shifted(`Digit${i}`, String(i), shift));

/** A row of keys that share its width evenly (the number row, the symbol row). */
const even = (keys: Key[]) => keys.map((k) => ({ ...k, units: ROW_UNITS / keys.length }));

const BACKQUOTE = shifted("Backquote", "`", "~");

const NUMBER_ROW = even([BACKQUOTE, ...DIGITS.slice(1), DIGITS[0], key("Backspace", "⌫")]);

const ARROWS = [key("ArrowLeft", "←"), key("ArrowUp", "↑"), key("ArrowDown", "↓"), key("ArrowRight", "→")];

/** The number row under Fn: Home, PgUp, PgDn and End on 1 to 4, the arrows on 7 to 0. */
const FN_NUMBER_ROW = even([
  BACKQUOTE,
  key("Home", "Home"),
  key("PageUp", "PgUp"),
  key("PageDown", "PgDn"),
  key("End", "End"),
  DIGITS[5],
  DIGITS[6],
  ...ARROWS,
  key("Backspace", "⌫"),
]);

/** The symbol row under Fn: F1 to F12, one on each key. */
const F_ROW = even(Array.from({ length: 12 }, (_, i) => key(`F${i + 1}`, `F${i + 1}`)));

const SYMBOL_ROW = even([
  key("Escape", "Esc"),
  shifted("BracketLeft", "[", "{"),
  shifted("BracketRight", "]", "}"),
  shifted("Backslash", "\\", "|"),
  shifted("Semicolon", ";", ":"),
  shifted("Quote", "'", '"'),
  shifted("Comma", ",", "<"),
  shifted("Period", ".", ">"),
  shifted("Slash", "/", "?"),
  shifted("Minus", "-", "_"),
  shifted("Equal", "=", "+"),
  // GNOME's screenshot key; it makes the row as long as the number row under it
  key("PrintScreen", "PrtSc"),
]);

/** The key after "m", where the iPhone keyboard has its delete key: Backspace, and forward
 *  Delete under Fn (as on the number row). Its id is its own, so a touch on it does not light
 *  the number row's key with the same code. */
const deleteKey = (fn: boolean): Key =>
  fn
    ? { ...key("Delete", "Del", 2.25), id: "DeleteRight" }
    : { ...key("Backspace", "⌫", 2.25), id: "BackspaceRight" };

function letterRows(fn: boolean): Key[][] {
  return [
    [key("Tab", "Tab", 1, { icon: "tab" }), ...letters("qwertyuiop")],
    [
      key("CapsLock", "Caps", 1.25),
      ...letters("asdfghjkl"),
      key("Enter", "Enter", 1.25, { icon: "enter" }),
    ],
    [mod("ShiftLeft", "Shift", 2.25, { icon: "shift" }), ...letters("zxcvbnm"), deleteKey(fn)],
  ];
}

/** Fn, on the left of Space, under the thumb. */
const FN: Key = { id: "Fn", code: 0, label: "Fn", units: 1, fn: true };

/** A second comma and period at the end of the space row, near the thumb, as on the iPhone
 *  keyboard. Their ids are their own, so a touch on one does not light the symbol row's. */
const NEAR_PUNCTUATION: Key[] = [
  { ...shifted("Comma", ",", "<"), id: "CommaRight" },
  { ...shifted("Period", ".", ">"), id: "PeriodRight" },
];

/** Dictation, the last key of the space row. */
const DICTATE: Key = { id: "Dictate", code: 0, label: "Dictate", units: 1, dictate: true, icon: "mic" };

const SPACE_ROW: Key[] = [
  mod("ControlLeft", "Ctrl", 1),
  mod("MetaLeft", "Win", 1, { icon: "win" }),
  mod("AltLeft", "Alt", 1),
  FN,
  key("Space", "", 4.5),
  ...NEAR_PUNCTUATION,
  DICTATE,
];

/** The six rows, top first: symbols, numbers, three of letters, space. `fn` puts F1–F12 in
 *  place of the symbol row, and Home, PgUp, PgDn, End and the arrows on number keys. */
export function keyRows(fn: boolean): Key[][] {
  return [fn ? F_ROW : SYMBOL_ROW, fn ? FN_NUMBER_ROW : NUMBER_ROW, ...letterRows(fn), SPACE_ROW];
}

/** The arrow keys a drag on Space types (`./keypad.ts`). */
export const ARROW_KEYS = { left: ARROWS[0], up: ARROWS[1], down: ARROWS[2], right: ARROWS[3] };
/** The evdev code of Space. */
export const SPACE = CODE_TO_EVDEV.Space;

/** The evdev codes of the Shift keys. */
export const SHIFTS: readonly number[] = [CODE_TO_EVDEV.ShiftLeft, CODE_TO_EVDEV.ShiftRight];
/** Whether a modifier locks on a double tap (`./modifiers.ts`): the Shift keys. */
export const isShift = (code: number) => SHIFTS.includes(code);
/** The evdev code of Caps Lock. */
export const CAPS_LOCK = CODE_TO_EVDEV.CapsLock;

/** What a key shows: the Shift label while Shift is down, and upper case letters when either
 *  Shift or Caps Lock is on (not both). */
export function keyLabel(k: Key, shift: boolean, caps: boolean): string {
  if (k.letter) return shift !== caps ? k.label.toUpperCase() : k.label;
  return shift && k.shift ? k.shift : k.label;
}
