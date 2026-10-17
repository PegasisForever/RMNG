import { expect, test } from "bun:test";

import {
  AutoLock,
  ENGAGE_MS,
  RELEASE_MS,
  freshInput,
  holdInput,
  letterbox,
  routeDrag,
  shapeToRgba,
  toMonitor,
  wheelMessages,
} from "./input";
import { CODE_TO_EVDEV, evdevFor } from "./keycodes";

test("no two codes share an evdev code", () => {
  const values = Object.values(CODE_TO_EVDEV);
  expect(new Set(values).size).toBe(values.length);
});

test("on a Mac, Command and Control swap; elsewhere they do not", () => {
  expect(evdevFor("MetaLeft", true)).toBe(29);
  expect(evdevFor("MetaRight", true)).toBe(97);
  expect(evdevFor("ControlLeft", true)).toBe(125);
  expect(evdevFor("ControlRight", true)).toBe(126);
  expect(evdevFor("KeyC", true)).toBe(46);
  expect(evdevFor("MetaLeft", false)).toBe(125);
  expect(evdevFor("ControlLeft", false)).toBe(29);
  expect(evdevFor("Nope", true)).toBeUndefined();
});

test("page points map through the letterbox to monitor pixels", () => {
  // A 2000×1000 monitor in a 1000×1000 CSS box of the same device pixels: scale 1/2, bars
  // of 250 above and below.
  const rect = { left: 10, top: 20, width: 1000, height: 1000 };
  const box = { w: 1000, h: 1000 };
  expect(letterbox(1000, 1000, 2000, 1000)).toEqual({ scale: 0.5, offX: 0, offY: 250 });
  expect(toMonitor(10, 270, rect, box, 2000, 1000)).toEqual({ x: 0, y: 0 });
  expect(toMonitor(510, 520, rect, box, 2000, 1000)).toEqual({ x: 1000, y: 500 });
  // Points in the bars fall outside the monitor (not clamped).
  expect(toMonitor(10, 20, rect, box, 2000, 1000).y).toBe(-500);
  // Bars at the sides for a tall box.
  const p = toMonitor(150, 50, { left: 0, top: 0, width: 300, height: 100 }, { w: 300, h: 100 }, 200, 100);
  expect(p).toEqual({ x: 100, y: 50 });
});

test("at device pixel ratio 1.5, a canvas of the monitor's device size maps 1:1", () => {
  // 1920×1080 device pixels in a 1280×720 CSS box: scale 1, no bars.
  expect(letterbox(1920, 1080, 1920, 1080)).toEqual({ scale: 1, offX: 0, offY: 0 });
  const rect = { left: 0, top: 0, width: 1280, height: 720 };
  expect(toMonitor(640, 360, rect, { w: 1920, h: 1080 }, 1920, 1080)).toEqual({ x: 960, y: 540 });
  // One device pixel too wide: the bar is a whole pixel, so the picture stays on the pixel
  // grid.
  expect(letterbox(1921, 1080, 1920, 1080)).toEqual({ scale: 1, offX: 1, offY: 0 });
});

test("a drag past the window edge routes to the neighbouring monitor", () => {
  const row = [
    { id: 0, x: 0, y: 0, width: 1920, height: 1080, primary: true },
    { id: 1, x: 1920, y: 0, width: 1920, height: 1080, primary: false },
  ];
  expect(routeDrag(row, 0, 10, 20)).toEqual({ id: 0, x: 10, y: 20 });
  expect(routeDrag(row, 0, 1920, 500)).toEqual({ id: 1, x: 0, y: 500 });
  expect(routeDrag(row, 1, -1, 300)).toEqual({ id: 0, x: 1919, y: 300 });
  // Past the outer edge, or above: pinned to the origin's edge.
  expect(routeDrag(row, 1, 2500, -40)).toEqual({ id: 1, x: 1919, y: 0 });
  // An offset neighbour is entered only where it overlaps.
  const offset = [row[0], { ...row[1], y: 600 }];
  expect(routeDrag(offset, 0, 1930, 700)).toEqual({ id: 1, x: 10, y: 100 });
  expect(routeDrag(offset, 0, 1930, 100)).toEqual({ id: 0, x: 1919, y: 100 });
  // An origin not in the layout routes nowhere.
  expect(routeDrag(row, 7, 0, 0)).toBeNull();
});

test("the cursor sprite is premultiplied BGRA, and comes out straight RGBA", () => {
  // One opaque red pixel (B=0 G=0 R=255 A=255), one half-transparent white (128,128,128,128).
  const bytes = String.fromCharCode(0, 0, 255, 255, 128, 128, 128, 128);
  const out = shapeToRgba({ width: 2, height: 1, hotspot_x: 0, hotspot_y: 0, rgba: btoa(bytes) });
  expect([...out]).toEqual([255, 0, 0, 255, 255, 255, 255, 128]);
});

test("auto lock engages after a held hide and releases after a held show", () => {
  const al = new AutoLock(0);
  al.onRemoteCursor(true, 100);
  expect(al.want(150)).toBe(false);
  expect(al.want(100 + ENGAGE_MS)).toBe(true);
  al.onRemoteCursor(false, 1000);
  expect(al.want(1100)).toBe(true);
  expect(al.want(1000 + RELEASE_MS)).toBe(false);
});

test("a manual release during a grab re-arms after the cursor shows", () => {
  const al = new AutoLock(0);
  al.onRemoteCursor(true, 0);
  expect(al.want(ENGAGE_MS)).toBe(true);
  expect(al.toggle(500)).toBe(false);
  expect(al.want(5000)).toBe(false);
  al.onRemoteCursor(false, 6000);
  expect(al.want(6000 + RELEASE_MS)).toBe(false);
  al.onRemoteCursor(true, 8000);
  expect(al.want(8000 + ENGAGE_MS)).toBe(true);
});

test("on a Mac, a wheel detent is one notch and a trackpad is finger scroll", () => {
  const rem = { x: 0, y: 0 };
  const detent = { deltaMode: 0, deltaX: 0, deltaY: 4.000244140625, wheelDeltaX: 0, wheelDeltaY: -12 };
  expect(wheelMessages(detent, rem, { os: "mac" })).toEqual([{ kind: "axis", axis: 0, step: 1 }]);
  // A fast spin: one event, still one notch.
  expect(wheelMessages({ ...detent, deltaY: -380.0232, wheelDeltaY: 1140 }, rem, { os: "mac" })).toEqual([
    { kind: "axis", axis: 0, step: -1 },
  ]);
  // Shift turns the wheel sideways in whole lines.
  const sideways = { deltaMode: 0, deltaX: 40, deltaY: 0, wheelDeltaX: -120, wheelDeltaY: 0, shiftKey: true };
  expect(wheelMessages(sideways, rem, { os: "mac" })).toEqual([{ kind: "axis", axis: 1, step: 1 }]);
  // A trackpad scrolls in whole pixels.
  const pad = { deltaMode: 0, deltaX: 0, deltaY: 7, wheelDeltaX: 0, wheelDeltaY: -21 };
  expect(wheelMessages(pad, rem, { os: "mac" })).toEqual([
    { kind: "axis_continuous", dx: 0, dy: 7, flags: 4 },
  ]);
});

test("on Linux, a wheel's whole deltas are notches and a touchpad's fractions finger scroll", () => {
  const rem = { x: 0, y: 0 };
  // A high-resolution wheel: eight events of 15 make one notch.
  const hires = { deltaMode: 0, deltaX: 0, deltaY: 15, wheelDeltaX: 0, wheelDeltaY: -15 };
  for (let i = 0; i < 7; i++) expect(wheelMessages(hires, rem, { os: "linux" })).toEqual([]);
  expect(wheelMessages(hires, rem, { os: "linux" })).toEqual([{ kind: "axis", axis: 0, step: 1 }]);
  // A plain notch at device pixel ratio 1.5: 120 / 1.5 = 80.
  const notch = { deltaMode: 0, deltaX: 0, deltaY: 80, wheelDeltaX: 0, wheelDeltaY: -120 };
  expect(wheelMessages(notch, rem, { os: "linux", dpr: 1.5 })).toEqual([{ kind: "axis", axis: 0, step: 1 }]);
  // Turning back starts from zero: a leftover fraction does not eat the first notch up.
  wheelMessages({ ...hires, deltaY: 60, wheelDeltaY: -60 }, rem, { os: "linux" });
  expect(wheelMessages({ ...notch, deltaY: -120, wheelDeltaY: 120 }, rem, { os: "linux" })).toEqual([
    { kind: "axis", axis: 0, step: -1 },
  ]);
  const pad = { deltaMode: 0, deltaX: 0, deltaY: 3.92578125, wheelDeltaX: 0, wheelDeltaY: -4 };
  expect(wheelMessages(pad, rem, { os: "linux" })).toEqual([
    { kind: "axis_continuous", dx: 0, dy: 3.92578125, flags: 4 },
  ]);
});

test("the layout's mode forces one device, and its speed scales both", () => {
  const rem = { x: 0, y: 0 };
  const notch = { deltaMode: 0, deltaX: 0, deltaY: 120, wheelDeltaX: 0, wheelDeltaY: -120 };
  const pad = { deltaMode: 0, deltaX: 0, deltaY: 3.5, wheelDeltaX: 0, wheelDeltaY: -4 };
  expect(wheelMessages(notch, rem, { os: "linux", mode: "trackpad" })).toEqual([
    { kind: "axis_continuous", dx: 0, dy: 120, flags: 4 },
  ]);
  expect(wheelMessages(pad, rem, { os: "linux", mode: "trackpad", scale: 2 })).toEqual([
    { kind: "axis_continuous", dx: 0, dy: 7, flags: 4 },
  ]);
  // Mouse mode counts a trackpad's travel in notches: 30 of these are one.
  for (let i = 0; i < 29; i++) expect(wheelMessages(pad, rem, { os: "linux", mode: "mouse" })).toEqual([]);
  expect(wheelMessages(pad, rem, { os: "linux", mode: "mouse" })).toEqual([{ kind: "axis", axis: 0, step: 1 }]);
  // Speed 50%: two notches make one step.
  expect(wheelMessages(notch, rem, { os: "linux", scale: 0.5 })).toEqual([]);
  expect(wheelMessages(notch, rem, { os: "linux", scale: 0.5 })).toEqual([{ kind: "axis", axis: 0, step: 1 }]);
  // Speed 200% on a Mac detent: two steps.
  const detent = { deltaMode: 0, deltaX: 0, deltaY: 4.000244140625, wheelDeltaX: 0, wheelDeltaY: -12 };
  expect(wheelMessages(detent, rem, { os: "mac", scale: 2 })).toEqual([{ kind: "axis", axis: 0, step: 2 }]);
  // Speed -100%: the same distance the other way.
  expect(wheelMessages(notch, rem, { os: "linux", scale: -1 })).toEqual([{ kind: "axis", axis: 0, step: -1 }]);
  expect(wheelMessages(pad, rem, { os: "linux", scale: -1 })).toEqual([
    { kind: "axis_continuous", dx: -0, dy: -3.5, flags: 4 },
  ]);
  // Speed 150%: two notches make three steps.
  expect(wheelMessages(notch, rem, { os: "linux", scale: 1.5 })).toEqual([{ kind: "axis", axis: 0, step: 1 }]);
  expect(wheelMessages(notch, rem, { os: "linux", scale: 1.5 })).toEqual([{ kind: "axis", axis: 0, step: 2 }]);
});

test("a mouse wheel sends notches, a trackpad sends finger scroll", () => {
  const rem = { x: 0, y: 0 };
  const wheel = { deltaMode: 0, deltaX: 0, deltaY: 100, wheelDeltaX: 0, wheelDeltaY: -120 };
  expect(wheelMessages(wheel, rem)).toEqual([{ kind: "axis", axis: 0, step: 1 }]);
  expect(wheelMessages({ ...wheel, wheelDeltaY: 240 }, rem)).toEqual([
    { kind: "axis", axis: 0, step: -2 },
  ]);
  expect(wheelMessages({ ...wheel, wheelDeltaY: 0, wheelDeltaX: -120 }, rem)).toEqual([
    { kind: "axis", axis: 1, step: 1 },
  ]);
  const pad = { deltaMode: 0, deltaX: 1.5, deltaY: -7.25, wheelDeltaX: -4, wheelDeltaY: 21 };
  expect(wheelMessages(pad, rem)).toEqual([
    { kind: "axis_continuous", dx: 1.5, dy: -7.25, flags: 4 },
  ]);
  // A tiny trackpad move rounds to wheelDelta 0: still finger scroll, not a lost notch.
  expect(wheelMessages({ ...pad, wheelDeltaX: 0, wheelDeltaY: 0 }, rem)).toHaveLength(1);
  // During a finger scroll, an event that looks like a notch is still finger scroll.
  expect(wheelMessages({ ...pad, wheelDeltaY: -120 }, rem, { fingers: true })).toEqual([
    { kind: "axis_continuous", dx: 1.5, dy: -7.25, flags: 4 },
  ]);
});

test("input held during a reconnect: moves merge, old input goes but releases stay", () => {
  let held = holdInput([], { kind: "pointer_move", monitor_id: 0, x: 1, y: 1 }, 0);
  held = holdInput(held, { kind: "pointer_move", monitor_id: 0, x: 2, y: 2 }, 10);
  held = holdInput(held, { kind: "button", button: 0x110, pressed: true }, 20);
  held = holdInput(held, { kind: "pointer_move", monitor_id: 0, x: 3, y: 3 }, 30);
  held = holdInput(held, { kind: "pointer_relative", dx: 1, dy: 2 }, 40);
  held = holdInput(held, { kind: "pointer_relative", dx: 3, dy: -1 }, 50);
  held = holdInput(held, { kind: "button", button: 0x110, pressed: false }, 60);
  expect(held.map((h) => h.msg)).toEqual([
    { kind: "pointer_move", monitor_id: 0, x: 2, y: 2 },
    { kind: "button", button: 0x110, pressed: true },
    { kind: "pointer_move", monitor_id: 0, x: 3, y: 3 },
    { kind: "pointer_relative", dx: 4, dy: 1 },
    { kind: "button", button: 0x110, pressed: false },
  ]);
  // After 2 s only the release is still worth sending.
  expect(freshInput(held, 2055).map((h) => h.msg)).toEqual([
    { kind: "button", button: 0x110, pressed: false },
  ]);
});
