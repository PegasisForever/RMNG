import { describe, expect, test } from "bun:test";

import { modeAfter } from "./inputMode";
import { isPagesChord } from "./shortcut";

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

  test("Super+Space, on every system", () => {
    for (const os of ["linux", "mac", "windows"] as const) expect(isPagesChord(key({ metaKey: true }), os)).toBe(true);
  });

  test("Command+Shift+Space on macOS", () => {
    expect(isPagesChord(key({ metaKey: true, shiftKey: true }), "mac")).toBe(true);
  });

  test("Ctrl+Alt+Space on Windows only", () => {
    expect(isPagesChord(key({ ctrlKey: true, altKey: true }), "windows")).toBe(true);
    expect(isPagesChord(key({ ctrlKey: true, altKey: true }), "linux")).toBe(false);
  });

  test("not Space, or Space alone, or with Ctrl, is not it", () => {
    expect(isPagesChord(key({ metaKey: true }, "KeyA"), "linux")).toBe(false);
    expect(isPagesChord(key({}), "linux")).toBe(false);
    expect(isPagesChord(key({ ctrlKey: true }), "linux")).toBe(false);
    expect(isPagesChord(key({ metaKey: true, ctrlKey: true }), "mac")).toBe(false);
  });
});
