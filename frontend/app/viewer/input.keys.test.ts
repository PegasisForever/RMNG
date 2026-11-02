// The keys `attachInput` sends for the page's key events. Separate file because it needs a DOM:
// `@happy-dom/global-registrator` puts one on the globals before `./input` is imported.
import { expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

import type { InputMsg } from "~/lib/wire/InputMsg";

if (!GlobalRegistrator.isRegistered) GlobalRegistrator.register();
// What happy-dom lacks that `attachInput` looks at
(globalThis as { ResizeObserverEntry?: unknown }).ResizeObserverEntry ??= class {};
MouseEvent.prototype.getModifierState ??= () => false;
const { attachInput } = await import("./input");

/** The page's input on a Mac, and what it sends, as "code down" / "code up". */
function mac(pasteByEvent = false) {
  const sent: string[] = [];
  // A Mac: `attachInput` reads the platform as it attaches
  const platform = Object.getOwnPropertyDescriptor(navigator, "platform");
  Object.defineProperty(navigator, "platform", { value: "MacIntel", configurable: true });
  const canvas = document.createElement("canvas");
  const input = attachInput({
    canvas,
    overlay: document.createElement("img"),
    monitor: 0,
    send: (m: InputMsg) => {
      if (m.kind === "key_code") sent.push(`${m.keycode} ${m.pressed ? "down" : "up"}`);
      if (m.kind === "button") sent.push(`button ${m.button} ${m.pressed ? "down" : "up"}`);
    },
    sendLocks: () => {},
    onGesture: () => {},
    pasteByEvent: () => pasteByEvent,
    chord: { is: () => false, holds: [], run: () => {} },
  });
  if (platform) Object.defineProperty(navigator, "platform", platform);
  else delete (navigator as { platform?: string }).platform;
  input.setLayout([{ id: 0, x: 0, y: 0, width: 1920, height: 1080, primary: true }]);
  const key = (type: "keydown" | "keyup", code: string, init: KeyboardEventInit = {}) =>
    window.dispatchEvent(new KeyboardEvent(type, { code, bubbles: true, cancelable: true, ...init }));
  return { sent, key, input, canvas };
}

// On a Mac, Command goes as Ctrl (29), and V is 47
test("on a Mac, Cmd+V is a tap of V: no keyup comes for V while Command is down, so the clone must not hold it", () => {
  const { sent, key, input } = mac();
  key("keydown", "MetaLeft", { metaKey: true });
  key("keydown", "KeyV", { metaKey: true });
  // V goes up with Command still down: macOS gives the page no keyup. The browser's own repeat
  // came once before that, and is one more tap.
  key("keydown", "KeyV", { metaKey: true, repeat: true });
  expect(sent).toEqual(["29 down", "47 down", "47 up", "47 down", "47 up"]);
  key("keyup", "MetaLeft");
  expect(sent.slice(5)).toEqual(["29 up"]);
  input.detach();
});

test("a key held before Command went down is held in the clone until it goes up", () => {
  const { sent, key, input } = mac();
  key("keydown", "ShiftLeft", { shiftKey: true });
  key("keydown", "KeyA", { shiftKey: true });
  key("keyup", "KeyA", { shiftKey: true });
  key("keyup", "ShiftLeft");
  expect(sent).toEqual(["42 down", "30 down", "30 up", "42 up"]);
  input.detach();
});

test("a paste chord whose V waits for the paste event is a tap too", () => {
  const { sent, key, input } = mac(true);
  key("keydown", "MetaLeft", { metaKey: true });
  key("keydown", "KeyV", { metaKey: true });
  // No paste event and no keyup: V goes once the wait is over, pressed and released
  return new Promise<void>((done) =>
    setTimeout(() => {
      expect(sent).toEqual(["29 down", "47 down", "47 up"]);
      input.detach();
      done();
    }, 300),
  );
});

test("detach lets go of the keys and the buttons the clone holds: the control panel opened over a held key", () => {
  const { sent, key, input, canvas } = mac();
  key("keydown", "ShiftLeft", { shiftKey: true });
  canvas.dispatchEvent(new MouseEvent("mousedown", { button: 0, shiftKey: true, bubbles: true, cancelable: true }));
  input.detach();
  expect(sent).toEqual(["42 down", "button 272 down", "42 up", "button 272 up"]);
});
