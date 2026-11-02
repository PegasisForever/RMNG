// The buttons `attachInput` sends for the page's mouse events. Separate file because it needs a
// DOM: `@happy-dom/global-registrator` puts one on the globals before `./input` is imported.
import { expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

import type { InputMsg } from "~/lib/wire/InputMsg";

if (!GlobalRegistrator.isRegistered) GlobalRegistrator.register();
// What happy-dom lacks that `attachInput` looks at
(globalThis as { ResizeObserverEntry?: unknown }).ResizeObserverEntry ??= class {};
const { attachInput } = await import("./input");

test("detach lets go of the buttons the clone holds: the control panel opened over a held button", () => {
  const sent: string[] = [];
  const canvas = document.createElement("canvas");
  const input = attachInput({
    canvas,
    overlay: document.createElement("img"),
    monitor: 0,
    send: (m: InputMsg) => {
      if (m.kind === "button") sent.push(`${m.button} ${m.pressed ? "down" : "up"}`);
    },
    onGesture: () => {},
  });
  input.setLayout([{ id: 0, x: 0, y: 0, width: 1920, height: 1080, primary: true }]);
  canvas.dispatchEvent(new MouseEvent("mousedown", { button: 0, bubbles: true, cancelable: true }));
  input.detach();
  expect(sent).toEqual(["272 down", "272 up"]);
});
