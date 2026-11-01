import { describe, expect, test } from "bun:test";

import { modeAfter } from "./inputMode";
import { isPagesChord, pagesChordLabel } from "./shortcut";

describe("modeAfter", () => {
  test("a finger or a pen is touch; a mouse click is mouse", () => {
    expect(modeAfter({ t: "pointerdown", pointerType: "touch" })).toBe("touch");
    expect(modeAfter({ t: "pointerdown", pointerType: "pen" })).toBe("touch");
    expect(modeAfter({ t: "pointerdown", pointerType: "mouse" })).toBe("mouse");
  });

  test("a mouse that moves is mouse; a finger's move says nothing", () => {
    expect(modeAfter({ t: "pointermove", pointerType: "mouse", moved: true })).toBe("mouse");
    expect(modeAfter({ t: "pointermove", pointerType: "mouse", moved: false })).toBeNull();
    expect(modeAfter({ t: "pointermove", pointerType: "touch", moved: true })).toBeNull();
  });

  test("a key is mouse, unless it types into a text box of the page", () => {
    expect(modeAfter({ t: "keydown", editable: false, repeat: false })).toBe("mouse");
    expect(modeAfter({ t: "keydown", editable: true, repeat: false })).toBeNull();
  });
});

describe("isPagesChord", () => {
  const key = (m: Partial<Record<"metaKey" | "ctrlKey" | "altKey" | "shiftKey", boolean>>, code = "Space") => ({
    code,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...m,
  });

  test("Super+Space on Linux and Windows", () => {
    expect(isPagesChord(key({ metaKey: true }), "linux")).toBe(true);
    expect(isPagesChord(key({ metaKey: true }), "windows")).toBe(true);
    expect(isPagesChord(key({ altKey: true }), "linux")).toBe(false);
  });

  test("Option+Space on macOS, not Command+Space (Spotlight)", () => {
    expect(isPagesChord(key({ altKey: true }), "mac")).toBe(true);
    expect(isPagesChord(key({ metaKey: true }), "mac")).toBe(false);
  });

  test("the old chords, another key, or more modifiers are not it", () => {
    expect(isPagesChord(key({ ctrlKey: true, altKey: true }), "linux")).toBe(false);
    expect(isPagesChord(key({ metaKey: true, shiftKey: true }), "mac")).toBe(false);
    expect(isPagesChord(key({ metaKey: true }, "KeyA"), "linux")).toBe(false);
    expect(isPagesChord(key({}), "linux")).toBe(false);
    expect(isPagesChord(key({ metaKey: true, altKey: true }), "windows")).toBe(false);
  });
});

describe("pagesChordLabel", () => {
  test("Option + Space on a Mac; Super or Windows + Space elsewhere, in fullscreen", () => {
    expect(pagesChordLabel("mac")).toBe("⌥ Option + Space");
    expect(pagesChordLabel("linux")).toBe("Super + Space (in fullscreen)");
    expect(pagesChordLabel("windows")).toBe("Windows + Space (in fullscreen)");
  });
});
