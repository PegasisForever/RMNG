import { describe, expect, test } from "bun:test";

import { CODE_TO_EVDEV } from "~/viewer/keycodes";

import { TermKeys, termBytes } from "./termKeys";

const none = { shift: false, ctrl: false, alt: false };

describe("termBytes", () => {
  test("letters, digits and symbols, with Shift and Caps Lock", () => {
    expect(termBytes("KeyA", none, false, false)).toBe("a");
    expect(termBytes("KeyA", { ...none, shift: true }, false, false)).toBe("A");
    expect(termBytes("KeyA", none, true, false)).toBe("A");
    expect(termBytes("KeyA", { ...none, shift: true }, true, false)).toBe("a");
    expect(termBytes("Digit1", { ...none, shift: true }, true, false)).toBe("!");
    expect(termBytes("Digit0", { ...none, shift: true }, false, false)).toBe(")");
    expect(termBytes("Slash", { ...none, shift: true }, false, false)).toBe("?");
    expect(termBytes("Space", none, false, false)).toBe(" ");
  });

  test("Ctrl makes control codes; Alt puts ESC first", () => {
    expect(termBytes("KeyC", { ...none, ctrl: true }, false, false)).toBe("\x03");
    expect(termBytes("BracketLeft", { ...none, ctrl: true }, false, false)).toBe("\x1b");
    expect(termBytes("Space", { ...none, ctrl: true }, false, false)).toBe("\x00");
    expect(termBytes("KeyB", { ...none, alt: true }, false, false)).toBe("\x1bb");
    expect(termBytes("KeyX", { ...none, ctrl: true, alt: true }, false, false)).toBe("\x1b\x18");
    expect(termBytes("Comma", { ...none, ctrl: true }, false, false)).toBeNull();
  });

  test("Enter, Backspace, Tab, Esc", () => {
    expect(termBytes("Enter", none, false, false)).toBe("\r");
    expect(termBytes("Backspace", none, false, false)).toBe("\x7f");
    expect(termBytes("Tab", none, false, false)).toBe("\t");
    expect(termBytes("Tab", { ...none, shift: true }, false, false)).toBe("\x1b[Z");
    expect(termBytes("Escape", none, false, false)).toBe("\x1b");
  });

  test("cursor keys follow the application mode; a modifier makes CSI 1;m", () => {
    expect(termBytes("ArrowUp", none, false, false)).toBe("\x1b[A");
    expect(termBytes("ArrowUp", none, false, true)).toBe("\x1bOA");
    expect(termBytes("ArrowLeft", { ...none, ctrl: true }, false, true)).toBe("\x1b[1;5D");
    expect(termBytes("Home", none, false, true)).toBe("\x1b[H");
    expect(termBytes("End", { ...none, shift: true }, false, false)).toBe("\x1b[1;2F");
  });

  test("function keys, Page Up/Down, Delete", () => {
    expect(termBytes("F1", none, false, false)).toBe("\x1bOP");
    expect(termBytes("F2", { ...none, shift: true }, false, false)).toBe("\x1b[1;2Q");
    expect(termBytes("F5", none, false, false)).toBe("\x1b[15~");
    expect(termBytes("F12", none, false, false)).toBe("\x1b[24~");
    expect(termBytes("PageUp", none, false, false)).toBe("\x1b[5~");
    expect(termBytes("Delete", { ...none, ctrl: true }, false, false)).toBe("\x1b[3;5~");
  });

  test("keys that type nothing", () => {
    expect(termBytes("PrintScreen", none, false, false)).toBeNull();
  });
});

describe("TermKeys", () => {
  const c = CODE_TO_EVDEV;
  test("held modifiers and Caps Lock apply to the next keys", () => {
    const k = new TermKeys();
    expect(k.key(c.ControlLeft, true, false)).toBeNull();
    expect(k.key(c.KeyC, true, false)).toBe("\x03");
    expect(k.key(c.KeyC, false, false)).toBeNull();
    k.key(c.ControlLeft, false, false);
    expect(k.key(c.KeyC, true, false)).toBe("c");
    k.key(c.CapsLock, true, false);
    k.key(c.CapsLock, false, false);
    expect(k.key(c.KeyC, true, false)).toBe("C");
    expect(k.key(c.MetaLeft, true, false)).toBeNull();
    expect(k.key(c.ArrowUp, true, true)).toBe("\x1bOA");
  });
});
