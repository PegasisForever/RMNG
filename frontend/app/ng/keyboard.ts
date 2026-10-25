// The /ng on-screen keyboard: a full Linux (US) keyboard in six rows of ROW_UNITS each. The
// symbols that sit beside the letters on a real keyboard have a row of their own, so the
// letters get the width. Every
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
  /** Drawn as this icon in place of its label. */
  icon?: "tab" | "enter" | "shift" | "win";
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

const NUMBER_ROW = even([key("Escape", "Esc"), ...DIGITS.slice(1), DIGITS[0], key("Backspace", "⌫")]);

const F_ROW = even([
  key("Escape", "Esc"),
  ...Array.from({ length: 12 }, (_, i) => key(`F${i + 1}`, `F${i + 1}`)),
  key("Delete", "Del"),
  key("Backspace", "⌫"),
]);

const SYMBOL_ROW = even([
  shifted("Backquote", "`", "~"),
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

const LETTER_ROWS = [
  [key("Tab", "Tab", 1, { icon: "tab" }), ...letters("qwertyuiop")],
  [
    key("CapsLock", "Caps", 1.25),
    ...letters("asdfghjkl"),
    key("Enter", "Enter", 1.25, { icon: "enter" }),
  ],
  [
    mod("ShiftLeft", "Shift", 2.25, { icon: "shift" }),
    ...letters("zxcvbnm"),
    mod("ShiftRight", "Shift", 2.25, { icon: "shift" }),
  ],
];

const ARROWS = [key("ArrowLeft", "←"), key("ArrowUp", "↑"), key("ArrowDown", "↓"), key("ArrowRight", "→")];
/** With Fn, the arrows are the keys a laptop puts on them. */
const FN_ARROWS = [key("Home", "Home"), key("PageUp", "PgUp"), key("PageDown", "PgDn"), key("End", "End")];

/** Fn, where a laptop has the right Alt and Ctrl. */
const FN: Key = { id: "Fn", code: 0, label: "Fn", units: 1, fn: true };

function spaceRow(fn: boolean): Key[] {
  return [
    mod("ControlLeft", "Ctrl", 1),
    mod("MetaLeft", "Win", 1, { icon: "win" }),
    mod("AltLeft", "Alt", 1),
    key("Space", "", 3.5),
    FN,
    ...(fn ? FN_ARROWS : ARROWS),
  ];
}

/** The six rows, top first: symbols, numbers, three of letters, space. `fn` puts F1–F12 in
 *  place of the number row and Home, PgUp, PgDn, End in place of the arrows. */
export function keyRows(fn: boolean): Key[][] {
  return [SYMBOL_ROW, fn ? F_ROW : NUMBER_ROW, ...LETTER_ROWS, spaceRow(fn)];
}

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
