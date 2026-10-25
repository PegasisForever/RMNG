// The /ng on-screen keyboard: a full Linux (US) keyboard in five rows of 15 units each. Every
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
};

/** Every row is this many units wide. */
export const ROW_UNITS = 15;

function key(id: string, label: string, units = 1, extra: Partial<Key> = {}): Key {
  const code = CODE_TO_EVDEV[id];
  if (code === undefined) throw new Error(`no evdev code for ${id}`);
  return { id, code, label, units, ...extra };
}

const shifted = (id: string, label: string, shift: string) => key(id, label, 1, { shift });
const letters = (s: string) => [...s].map((c) => key(`Key${c.toUpperCase()}`, c, 1, { letter: true }));
const mod = (id: string, label: string, units: number) => key(id, label, units, { mod: true });

const DIGITS = [...")!@#$%^&*("].map((shift, i) => shifted(`Digit${i}`, String(i), shift));

const NUMBER_ROW = [
  key("Escape", "Esc"),
  shifted("Backquote", "`", "~"),
  ...DIGITS.slice(1),
  DIGITS[0],
  shifted("Minus", "-", "_"),
  shifted("Equal", "=", "+"),
  key("Backspace", "⌫"),
];

const F_ROW = [
  key("Escape", "Esc"),
  ...Array.from({ length: 12 }, (_, i) => key(`F${i + 1}`, `F${i + 1}`)),
  key("Delete", "Del"),
  key("Backspace", "⌫"),
];

const LETTER_ROWS = [
  [
    key("Tab", "Tab", 1.5),
    ...letters("qwertyuiop"),
    shifted("BracketLeft", "[", "{"),
    shifted("BracketRight", "]", "}"),
    key("Backslash", "\\", 1.5, { shift: "|" }),
  ],
  [
    key("CapsLock", "Caps", 1.75),
    ...letters("asdfghjkl"),
    shifted("Semicolon", ";", ":"),
    shifted("Quote", "'", '"'),
    key("Enter", "Enter", 2.25),
  ],
  [
    mod("ShiftLeft", "Shift", 2.25),
    ...letters("zxcvbnm"),
    shifted("Comma", ",", "<"),
    shifted("Period", ".", ">"),
    shifted("Slash", "/", "?"),
    mod("ShiftRight", "Shift", 2.75),
  ],
];

const ARROWS = [key("ArrowLeft", "←"), key("ArrowUp", "↑"), key("ArrowDown", "↓"), key("ArrowRight", "→")];
/** With Fn, the arrows are the keys a laptop puts on them. */
const FN_ARROWS = [key("Home", "Home"), key("PageUp", "PgUp"), key("PageDown", "PgDn"), key("End", "End")];

function spaceRow(fn: boolean): Key[] {
  return [
    mod("ControlLeft", "Ctrl", 1.25),
    mod("MetaLeft", "Win", 1.25),
    mod("AltLeft", "Alt", 1.25),
    key("Space", "", 4.75),
    mod("AltRight", "Alt", 1.25),
    mod("ControlRight", "Ctrl", 1.25),
    ...(fn ? FN_ARROWS : ARROWS),
  ];
}

/** The five rows, top first. `fn` puts F1–F12 in place of the number row and Home, PgUp,
 *  PgDn, End in place of the arrows. */
export function keyRows(fn: boolean): Key[][] {
  return [fn ? F_ROW : NUMBER_ROW, ...LETTER_ROWS, spaceRow(fn)];
}

/** The evdev codes of the Shift keys. */
export const SHIFTS: readonly number[] = [CODE_TO_EVDEV.ShiftLeft, CODE_TO_EVDEV.ShiftRight];
/** The evdev code of Caps Lock. */
export const CAPS_LOCK = CODE_TO_EVDEV.CapsLock;

/** What a key shows: the Shift label while Shift is down, and upper case letters when either
 *  Shift or Caps Lock is on (not both). */
export function keyLabel(k: Key, shift: boolean, caps: boolean): string {
  if (k.letter) return shift !== caps ? k.label.toUpperCase() : k.label;
  return shift && k.shift ? k.shift : k.label;
}
