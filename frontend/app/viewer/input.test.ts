import { expect, test } from "bun:test";

import { AutoLock, ENGAGE_MS, RELEASE_MS, shapeToRgba, toMonitor, wheelMessages } from "./input";
import { CODE_TO_EVDEV } from "./keycodes";

test("key codes map to Linux evdev codes", () => {
  expect(CODE_TO_EVDEV.Escape).toBe(1); // KEY_ESC
  expect(CODE_TO_EVDEV.KeyA).toBe(30);
  expect(CODE_TO_EVDEV.KeyC).toBe(46);
  expect(CODE_TO_EVDEV.KeyV).toBe(47);
  expect(CODE_TO_EVDEV.ShiftLeft).toBe(42);
  expect(CODE_TO_EVDEV.ControlLeft).toBe(29);
  expect(CODE_TO_EVDEV.AltRight).toBe(100);
  expect(CODE_TO_EVDEV.Enter).toBe(28);
  expect(CODE_TO_EVDEV.NumpadEnter).toBe(96);
  expect(CODE_TO_EVDEV.ArrowLeft).toBe(105);
  expect(CODE_TO_EVDEV.F12).toBe(88);
  expect(CODE_TO_EVDEV.IntlBackslash).toBe(86);
  // Command, as in the native macOS viewer's kvk_evdev table.
  expect(CODE_TO_EVDEV.MetaLeft).toBe(125);
  expect(CODE_TO_EVDEV.MetaRight).toBe(126);
});

test("no two codes share an evdev code", () => {
  const values = Object.values(CODE_TO_EVDEV);
  expect(new Set(values).size).toBe(values.length);
});

test("page points map through the letterbox to monitor pixels", () => {
  // A 1920×1080 monitor in a 1000×1000 box: scale 1000/1920, bars above and below.
  const rect = { left: 10, top: 20, width: 1000, height: 1000 };
  const s = 1000 / 1920;
  const barY = (1000 - 1080 * s) / 2;
  const corner = toMonitor(10, 20 + barY, rect, 1920, 1080);
  expect(corner.x).toBeCloseTo(0);
  expect(corner.y).toBeCloseTo(0);
  const mid = toMonitor(510, 520, rect, 1920, 1080);
  expect(mid.x).toBeCloseTo(960);
  expect(mid.y).toBeCloseTo(540);
  // Points in the bars clamp to the monitor's edge.
  expect(toMonitor(10, 20, rect, 1920, 1080).y).toBe(0);
  expect(toMonitor(2000, 2000, rect, 1920, 1080)).toEqual({ x: 1920, y: 1080 });
  // Bars at the sides for a tall box; same answer at any device pixel ratio (CSS pixels in).
  const p = toMonitor(150, 50, { left: 0, top: 0, width: 300, height: 100 }, 200, 100);
  expect(p).toEqual({ x: 100, y: 50 });
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
});
