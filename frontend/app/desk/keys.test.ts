// The keys' one owner (`./keys.ts`), through the calls the page makes: who has the keys, and
// which side each key reaches. Needs a DOM: `@happy-dom/global-registrator` puts one on the
// globals before `./keys` is imported.
import { afterEach, describe, expect, mock, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

import type { InputMsg } from "~/lib/wire/InputMsg";

import type { CloneKeys, Fullscreen, Keys, Os } from "./keys";

if (!GlobalRegistrator.isRegistered) GlobalRegistrator.register();
// What happy-dom lacks that the keys look at
MouseEvent.prototype.getModifierState ??= () => false;
const { attachKeys, controlPanelChordLabel, isControlPanelChord } = await import("./keys");

let attached: Keys | null = null;
afterEach(() => {
  attached?.detach();
  attached = null;
  document.body.innerHTML = "";
});

/** The keys on `os`, with a fake fullscreen. `sent`: what reached a clone, as "code down" /
 *  "code up" (with the clone's name when it is not "clone"); `did`: everything else, in order. */
function setup(o: { os?: Os; fullscreen?: boolean; escape?: boolean } = {}) {
  const sent: string[] = [];
  const did: string[] = [];
  let full = o.fullscreen ?? false;
  const watchers = new Set<() => void>();
  const fullscreen: Fullscreen = {
    on: () => full,
    toggle: () => did.push("toggle fullscreen"),
    lock: (on) => did.push(on ? "lock" : "unlock"),
    watch(f) {
      watchers.add(f);
      return () => watchers.delete(f);
    },
  };
  const setFullscreen = (on: boolean) => {
    full = on;
    for (const f of watchers) f();
  };
  const keys = attachKeys(
    {
      chord: () => did.push("chord"),
      escape: () => {
        did.push("escape");
        return o.escape ?? true;
      },
    },
    { os: o.os ?? "linux", fullscreen },
  );
  attached = keys;
  const clone = (c: { name?: string; desktop?: boolean; pasteByEvent?: boolean } = {}): CloneKeys => ({
    send: (m: InputMsg) => {
      if (m.kind === "key_code") sent.push(`${c.name ? `${c.name} ` : ""}${m.keycode} ${m.pressed ? "down" : "up"}`);
    },
    sendLocks: (caps) => did.push(`caps ${caps}`),
    clipboard: {
      onCopyKey: (e) => did.push(`copy key ${e.code}`),
      onGesture: () => did.push("gesture"),
      pasteByEvent: () => c.pasteByEvent ?? false,
    },
    mouse: { desktop: () => c.desktop ?? true, pointerLock: (chord) => did.push(`pointer lock ${chord}`) },
  });
  const key = (type: "keydown" | "keyup", code: string, init: KeyboardEventInit = {}, target: EventTarget = window) => {
    const e = new KeyboardEvent(type, { code, bubbles: true, cancelable: true, ...init });
    target.dispatchEvent(e);
    return e;
  };
  return { sent, did, keys, clone, key, setFullscreen };
}

// Evdev: Esc 1, A 30, C 46, V 47, Space 57, Ctrl 29, Shift 42, Alt 56, Super 125

describe("who has the keys", () => {
  test("an owner change lets go of every key the clone holds", () => {
    const { sent, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "ShiftLeft", { shiftKey: true });
    key("keydown", "KeyA", { shiftKey: true });
    keys.give(null);
    expect(sent).toEqual(["42 down", "30 down", "42 up", "30 up"]);
    // Their keyups come to the page now: nothing more for the clone
    key("keyup", "KeyA", { shiftKey: true });
    key("keyup", "ShiftLeft");
    expect(sent).toHaveLength(4);
  });

  test("a new clone in place of another: the old one lets go of what it holds", () => {
    const { sent, keys, clone, key } = setup();
    keys.give(clone({ name: "old" }));
    key("keydown", "ShiftLeft", { shiftKey: true });
    keys.give(clone({ name: "new" }));
    key("keydown", "KeyA");
    expect(sent).toEqual(["old 42 down", "old 42 up", "new 30 down"]);
  });

  test("a window that loses the focus lets go of the clone's keys", () => {
    const { sent, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "ControlLeft", { ctrlKey: true });
    window.dispatchEvent(new Event("blur"));
    expect(sent).toEqual(["29 down", "29 up"]);
  });

  test("a key the page has never reaches the clone; one the clone has never reaches the page", () => {
    const { sent, did, keys, clone, key } = setup();
    key("keydown", "KeyA");
    expect(sent).toEqual([]);
    keys.give(clone());
    key("keydown", "Escape");
    expect(sent).toEqual(["1 down"]);
    expect(did).not.toContain("escape");
  });
});

describe("Escape", () => {
  test("Escape while the page has the keys does not reach the clone: the page's Escape runs", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone());
    keys.give(null);
    const e = key("keydown", "Escape", { key: "Escape" });
    expect(did).toEqual(["escape"]);
    expect(e.defaultPrevented).toBe(true);
    expect(sent).toEqual([]);
  });

  test("an Escape the page's Escape did nothing with goes on", () => {
    const { did, key } = setup({ escape: false });
    const e = key("keydown", "Escape", { key: "Escape" });
    expect(did).toEqual(["escape"]);
    expect(e.defaultPrevented).toBe(false);
  });

  test("a dialog or a menu has its own Escape, and a key the page took is the page's", () => {
    const { did, key } = setup();
    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    document.body.append(dialog);
    key("keydown", "Escape", { key: "Escape" });
    dialog.remove();
    const field = document.createElement("div");
    field.addEventListener("keydown", (e) => e.preventDefault());
    document.body.append(field);
    key("keydown", "Escape", { key: "Escape" }, field);
    expect(did).toEqual([]);
  });
});

describe("the control panel's chord", () => {
  test("the chord runs in both owners, and none of its keys reaches the clone", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "MetaLeft", { metaKey: true });
    const e = key("keydown", "Space", { metaKey: true });
    expect(e.defaultPrevented).toBe(true);
    // The browser's repeat is the same press
    key("keydown", "Space", { metaKey: true, repeat: true });
    key("keyup", "Space", { metaKey: true });
    key("keyup", "MetaLeft");
    keys.give(null);
    key("keydown", "Space", { metaKey: true });
    expect(did.filter((d) => d === "chord")).toHaveLength(2);
    expect(sent).toEqual([]);
  });

  test("the chord lets go of what the clone held with it", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "ShiftLeft", { shiftKey: true });
    key("keydown", "Space", { metaKey: true, shiftKey: false });
    expect(sent).toEqual(["42 down", "42 up"]);
    expect(did).toContain("chord");
  });

  test("on a Mac it is Option+Space; Command+Space goes to the clone", () => {
    const { sent, did, keys, clone, key } = setup({ os: "mac" });
    keys.give(clone());
    key("keydown", "AltLeft", { altKey: true });
    key("keydown", "Space", { altKey: true });
    key("keyup", "AltLeft");
    expect(did).toContain("chord");
    expect(sent).toEqual([]);
    key("keydown", "Space", { metaKey: true });
    expect(sent).toEqual(["57 down", "57 up"]);
  });

  test("its modifier alone is a tap in the clone, and before another key it is pressed", () => {
    const { sent, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "MetaLeft", { metaKey: true });
    key("keyup", "MetaLeft");
    expect(sent).toEqual(["125 down", "125 up"]);
    key("keydown", "MetaLeft", { metaKey: true });
    key("keydown", "KeyA", { metaKey: true });
    expect(sent.slice(2)).toEqual(["125 down", "30 down"]);
  });

  test("its modifier is pressed in the clone by a click", () => {
    const { sent, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "MetaLeft", { metaKey: true });
    window.dispatchEvent(new MouseEvent("mousedown", { metaKey: true }));
    expect(sent).toEqual(["125 down"]);
  });

  test("Super+Space on Linux and Windows, Option+Space on macOS, not Command+Space (Spotlight)", () => {
    const k = (m: Partial<Record<"metaKey" | "ctrlKey" | "altKey" | "shiftKey", boolean>>, code = "Space") => ({
      code,
      metaKey: false,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      ...m,
    });
    expect(isControlPanelChord(k({ metaKey: true }), "linux")).toBe(true);
    expect(isControlPanelChord(k({ metaKey: true }), "windows")).toBe(true);
    expect(isControlPanelChord(k({ altKey: true }), "linux")).toBe(false);
    expect(isControlPanelChord(k({ altKey: true }), "mac")).toBe(true);
    expect(isControlPanelChord(k({ metaKey: true }), "mac")).toBe(false);
    // The old chords, another key, or more modifiers are not it
    expect(isControlPanelChord(k({ ctrlKey: true, altKey: true }), "linux")).toBe(false);
    expect(isControlPanelChord(k({ metaKey: true, shiftKey: true }), "mac")).toBe(false);
    expect(isControlPanelChord(k({ metaKey: true }, "KeyA"), "linux")).toBe(false);
    expect(isControlPanelChord(k({}), "linux")).toBe(false);
    expect(isControlPanelChord(k({ metaKey: true, altKey: true }), "windows")).toBe(false);
  });

  test("its label: Option + Space on a Mac; Super or Windows + Space elsewhere, in fullscreen", () => {
    expect(controlPanelChordLabel("mac")).toBe("⌥ Option + Space");
    expect(controlPanelChordLabel("linux")).toBe("Super + Space (in fullscreen)");
    expect(controlPanelChordLabel("windows")).toBe("Windows + Space (in fullscreen)");
  });
});

describe("the terminal", () => {
  test("a key typed into the terminal is the terminal's, but the chord is the page's", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone());
    const xterm = document.createElement("textarea");
    document.body.append(xterm);
    key("keydown", "KeyA", {}, xterm);
    key("keydown", "Space", { metaKey: true }, xterm);
    expect(sent).toEqual([]);
    expect(did).toEqual(["chord"]);
  });

  test("with no desktop here (a headless clone), no key goes to the clone", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone({ desktop: false }));
    key("keydown", "KeyA");
    key("keyup", "KeyA");
    expect(sent).toEqual([]);
    expect(did).toEqual([]);
  });
});

describe("a Mac's edit keys", () => {
  const exec = mock((_cmd: string) => true);
  (document as { execCommand: unknown }).execCommand = exec;
  afterEach(() => exec.mockClear());

  test("Mac Cmd+C with the page owning the keys runs copy, while the keyboard is locked", () => {
    const { sent, key } = setup({ os: "mac", fullscreen: true });
    const e = key("keydown", "KeyC", { metaKey: true });
    expect(exec.mock.calls).toEqual([["copy"]]);
    expect(e.defaultPrevented).toBe(true);
    expect(sent).toEqual([]);
  });

  test("cut, select all; undo and redo in a text box only", () => {
    const { key } = setup({ os: "mac", fullscreen: true });
    key("keydown", "KeyX", { metaKey: true });
    key("keydown", "KeyA", { metaKey: true });
    key("keydown", "KeyZ", { metaKey: true });
    const box = document.createElement("input");
    document.body.append(box);
    box.focus();
    key("keydown", "KeyZ", { metaKey: true }, box);
    key("keydown", "KeyZ", { metaKey: true, shiftKey: true }, box);
    expect(exec.mock.calls).toEqual([["cut"], ["selectAll"], ["undo"], ["redo"]]);
  });

  test("not with the keyboard unlocked, nor a key the page took, nor another chord, nor off a Mac", () => {
    const { key, setFullscreen } = setup({ os: "mac" });
    key("keydown", "KeyC", { metaKey: true });
    setFullscreen(true);
    const editor = document.createElement("div");
    editor.addEventListener("keydown", (e) => e.preventDefault());
    document.body.append(editor);
    key("keydown", "KeyC", { metaKey: true }, editor);
    key("keydown", "KeyC", { metaKey: true, altKey: true });
    key("keydown", "KeyV", { metaKey: true, shiftKey: true });
    key("keydown", "KeyB", { metaKey: true });
    expect(exec).not.toHaveBeenCalled();
    attached?.detach();
    setup({ os: "linux", fullscreen: true }).key("keydown", "KeyC", { metaKey: true });
    expect(exec).not.toHaveBeenCalled();
  });

  test("with the clone owning the keys, Cmd+C goes to it as Ctrl+C, and the clipboard hears of it first", () => {
    const { sent, did, keys, clone, key } = setup({ os: "mac", fullscreen: true });
    keys.give(clone());
    key("keydown", "MetaLeft", { metaKey: true });
    key("keydown", "KeyC", { metaKey: true });
    expect(exec).not.toHaveBeenCalled();
    expect(sent).toEqual(["29 down", "46 down", "46 up"]);
    expect(did.filter((d) => d === "gesture" || d.startsWith("copy")).slice(-2)).toEqual(["copy key KeyC", "gesture"]);
  });
});

describe("the clone's keys", () => {
  // On a Mac, Command goes as Ctrl (29)
  test("on a Mac, Cmd+V is a tap of V: no keyup comes for V while Command is down, so the clone must not hold it", () => {
    const { sent, keys, clone, key } = setup({ os: "mac" });
    keys.give(clone());
    key("keydown", "MetaLeft", { metaKey: true });
    key("keydown", "KeyV", { metaKey: true });
    // V goes up with Command still down: macOS gives the page no keyup. The browser's own repeat
    // came once before that, and is one more tap.
    key("keydown", "KeyV", { metaKey: true, repeat: true });
    expect(sent).toEqual(["29 down", "47 down", "47 up", "47 down", "47 up"]);
    key("keyup", "MetaLeft");
    expect(sent.slice(5)).toEqual(["29 up"]);
  });

  test("a key held before Command went down is held in the clone until it goes up", () => {
    const { sent, keys, clone, key } = setup({ os: "mac" });
    keys.give(clone());
    key("keydown", "ShiftLeft", { shiftKey: true });
    key("keydown", "KeyA", { shiftKey: true });
    key("keyup", "KeyA", { shiftKey: true });
    key("keyup", "ShiftLeft");
    expect(sent).toEqual(["42 down", "30 down", "30 up", "42 up"]);
  });

  test("a paste chord whose V waits for the paste event is a tap too", async () => {
    const { sent, keys, clone, key } = setup({ os: "mac" });
    keys.give(clone({ pasteByEvent: true }));
    key("keydown", "MetaLeft", { metaKey: true });
    const e = key("keydown", "KeyV", { metaKey: true });
    // Its default action fires the paste event
    expect(e.defaultPrevented).toBe(false);
    expect(sent).toEqual(["29 down"]);
    // No paste event and no keyup: V goes once the wait is over, pressed and released
    await new Promise((done) => setTimeout(done, 300));
    expect(sent).toEqual(["29 down", "47 down", "47 up"]);
  });

  test("a paste chord's V goes right after its paste event", async () => {
    const { sent, keys, clone, key } = setup();
    keys.give(clone({ pasteByEvent: true }));
    key("keydown", "ControlLeft", { ctrlKey: true });
    key("keydown", "KeyV", { ctrlKey: true });
    window.dispatchEvent(new Event("paste"));
    await new Promise((done) => setTimeout(done, 10));
    expect(sent).toEqual(["29 down", "47 down"]);
  });

  test("a modifier whose keyup never came is let go by the next key or mouse move that says it is up", () => {
    const { sent, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "ControlLeft", { ctrlKey: true });
    key("keydown", "KeyA");
    key("keydown", "ShiftLeft", { shiftKey: true });
    window.dispatchEvent(new MouseEvent("pointermove", { shiftKey: false }));
    expect(sent).toEqual(["29 down", "29 up", "30 down", "42 down", "42 up"]);
  });

  test("Caps Lock is sent as its state, and a letter brings the clone's in step first", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "CapsLock");
    key("keydown", "KeyA");
    expect(sent).toEqual(["30 down"]);
    expect(did.filter((d) => d.startsWith("caps"))).toEqual(["caps false", "caps false"]);
  });

  test("Ctrl+Alt+G and Ctrl+Alt+P are the pointer lock's, after the clone lets go of Ctrl and Alt", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone());
    key("keydown", "ControlLeft", { ctrlKey: true });
    key("keydown", "AltLeft", { ctrlKey: true, altKey: true });
    key("keydown", "KeyG", { ctrlKey: true, altKey: true });
    key("keydown", "KeyP", { ctrlKey: true, altKey: true });
    expect(sent).toEqual(["29 down", "56 down", "29 up", "56 up"]);
    expect(did.filter((d) => d.startsWith("pointer"))).toEqual(["pointer lock toggle", "pointer lock off"]);
  });
});

describe("fullscreen and the Keyboard Lock", () => {
  test("every key is locked while the page is fullscreen, whoever has the keys", () => {
    const { did, keys, clone, setFullscreen } = setup({ fullscreen: true });
    expect(did).toEqual(["lock"]);
    keys.give(clone());
    keys.give(null);
    expect(did).toEqual(["lock"]);
    setFullscreen(false);
    setFullscreen(true);
    expect(did).toEqual(["lock", "unlock", "lock"]);
  });

  test("F11 with the clone's keys turns the page's fullscreen on or off", () => {
    const { sent, did, keys, clone, key } = setup();
    keys.give(clone());
    const e = key("keydown", "F11");
    expect(e.defaultPrevented).toBe(true);
    expect(did).toContain("toggle fullscreen");
    expect(sent).toEqual([]);
  });
});
