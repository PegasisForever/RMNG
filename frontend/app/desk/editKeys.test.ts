import { describe, expect, test } from "bun:test";

import { editAction } from "./editKeys";

const key = (code: string, o: { meta?: boolean; ctrl?: boolean; alt?: boolean; shift?: boolean } = {}) => ({
  code,
  metaKey: o.meta ?? true,
  ctrlKey: o.ctrl ?? false,
  altKey: o.alt ?? false,
  shiftKey: o.shift ?? false,
});

describe("editAction", () => {
  test("Cmd with C, X, V, A, Z, and Shift+Z", () => {
    expect(editAction(key("KeyC"))).toBe("copy");
    expect(editAction(key("KeyX"))).toBe("cut");
    expect(editAction(key("KeyV"))).toBe("paste");
    expect(editAction(key("KeyA"))).toBe("selectAll");
    expect(editAction(key("KeyZ"))).toBe("undo");
    expect(editAction(key("KeyZ", { shift: true }))).toBe("redo");
  });
  test("not without Cmd, nor with Ctrl or Option, nor another key", () => {
    expect(editAction(key("KeyC", { meta: false }))).toBeNull();
    expect(editAction(key("KeyC", { ctrl: true }))).toBeNull();
    expect(editAction(key("KeyV", { alt: true }))).toBeNull();
    expect(editAction(key("KeyV", { shift: true }))).toBeNull();
    expect(editAction(key("KeyB"))).toBeNull();
  });
});
