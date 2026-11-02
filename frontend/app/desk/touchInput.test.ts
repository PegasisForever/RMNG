import { beforeEach, describe, expect, test } from "bun:test";

import type { InputMsg } from "~/lib/wire/InputMsg";
import { BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, CODE_TO_EVDEV } from "~/viewer/keycodes";
import type { TermApi } from "~/viewer/terminal";

import { desktopSink, terminalSink } from "./cloneSink";
import { keyRows } from "./keyboard";
import { HOLD_MS, SWIPE_PX, TAP_WAIT_MS } from "./keypad";
import { CLIP_NOTE_MS, type ClipKeys, type CloneSink, type DictationEvents, TouchInput } from "./touchInput";

const find = (id: string, fn = false) => keyRows(fn).flat().find((k) => k.id === id)!;
const A = find("KeyA");
const C = find("KeyC");
const CTRL = find("ControlLeft");
const SHIFT = find("ShiftLeft");
const CAPS = find("CapsLock");
const FN = find("Fn");
const DICTATE = find("Dictate");
const TO_LOCAL = find("ClipToLocal", true);
const TO_CLONE = find("ClipToClone", true);

const NAMES = new Map(Object.entries(CODE_TO_EVDEV).map(([id, code]) => [code, id]));
const BUTTONS = new Map([
  [BTN_LEFT, "BTN_LEFT"],
  [BTN_RIGHT, "BTN_RIGHT"],
  [BTN_MIDDLE, "BTN_MIDDLE"],
]);
const ud = (pressed: boolean) => (pressed ? "↓" : "↑");

/** A clock run by hand: `advance` moves the time and fires what is due. */
function handClock() {
  let jobs: { at: number; fn: () => void }[] = [];
  let now = 0;
  return {
    now: () => now,
    after(ms: number, fn: () => void) {
      const job = { at: now + ms, fn };
      jobs.push(job);
      return () => {
        jobs = jobs.filter((j) => j !== job);
      };
    },
    advance(ms: number) {
      now += ms;
      const due = jobs.filter((j) => j.at <= now);
      jobs = jobs.filter((j) => j.at > now);
      for (const j of due) j.fn();
    },
  };
}

/** A session that writes down what it is sent, as short words. */
function recordingSession(log: string[]) {
  return {
    send(m: InputMsg) {
      if (m.kind === "key_code") log.push(`${NAMES.get(m.keycode)}${ud(m.pressed)}`);
      else if (m.kind === "button") log.push(`${BUTTONS.get(m.button)}${ud(m.pressed)}`);
      else if (m.kind === "key") {
        const k = m.keysym < 0x100 ? `'${String.fromCharCode(m.keysym)}'` : `0x${m.keysym.toString(16)}`;
        log.push(`${k}${ud(m.pressed)}`);
      }
      else if (m.kind === "pointer_move") log.push(`move ${m.x},${m.y}`);
      else if (m.kind === "axis_continuous") log.push(m.flags & 1 ? "scroll end" : `scroll ${m.dy}`);
      else log.push(m.kind);
    },
  };
}

/** A terminal that writes down what it gets. */
function recordingTerm(log: string[]): TermApi {
  return {
    input: (data) => log.push(JSON.stringify(data)),
    mouse: (type, x, y, init) =>
      log.push(`${type} ${x},${y}${init.ctrlKey ? " ctrl" : ""}${init.shiftKey ? " shift" : ""}`),
    wheel: (deltaY) => log.push(`wheel ${deltaY}`),
    size: () => ({ width: 800, height: 400 }),
    appCursor: () => false,
  };
}

let log: string[];
let clock: ReturnType<typeof handClock>;
let input: TouchInput;
let clip: ClipKeys | null;
let dictation: DictationEvents;
let zooms: number[];
let cursor: { x: number; y: number };

/** The touch input on `sink`. */
function make(sink: CloneSink) {
  input = new TouchInput({
    sink: () => sink,
    clock,
    clipboard: () => clip,
    dictation: (on) => {
      dictation = on;
      let state: "off" | "listening" = "off";
      return {
        get current() {
          return state;
        },
        toggle() {
          state = state === "off" ? "listening" : "off";
          on.state(state);
        },
        stop() {},
      };
    },
    zoom: (f) => zooms.push(f),
  });
}

const desktop = () =>
  make(
    desktopSink(
      () => recordingSession(log),
      () => ({
        monitor: 0,
        width: 1000,
        height: 800,
        px: 1,
        scrollSpeed: 100,
        cursor,
        moved: (x, y) => (cursor = { x, y }),
      }),
    ),
  );
const terminal = () => make(terminalSink(() => recordingTerm(log), () => {}));

beforeEach(() => {
  log = [];
  clock = handClock();
  clip = null;
  zooms = [];
  cursor = { x: 500, y: 400 };
});

let ids = 0;
/** A finger taps `key` on the keys. */
function tap(key: (typeof A)) {
  const id = ++ids;
  input.keyTouch("down", id, key, 100, 100);
  clock.advance(50);
  input.keyTouch("up", id, key, 100, 100);
}
/** A finger taps the trackpad: a click, once the wait for a second touch is over. */
function padTap() {
  const id = ++ids;
  input.padTouch("down", id, 300, 300);
  clock.advance(50);
  input.padTouch("up", id, 300, 300);
  clock.advance(TAP_WAIT_MS);
}
/** Three fingers on the trackpad go (dx, dy) together. */
function swipe(dx: number, dy: number) {
  const at = [100, 160, 220].map((x) => ({ id: ++ids, x }));
  for (const f of at) input.padTouch("down", f.id, f.x, 300);
  for (let s = 1; s <= 4; s++) {
    clock.advance(20);
    for (const f of at) input.padTouch("move", f.id, f.x + (dx * s) / 4, 300 + (dy * s) / 4);
  }
  for (const f of at) input.padTouch("up", f.id, f.x, 300);
}
/** The clipboard's promises settle. */
const settle = () => new Promise((r) => setTimeout(r, 0));

describe("TouchInput on the desktop", () => {
  beforeEach(desktop);

  test("a key tapped goes down and up", () => {
    tap(A);
    expect(log).toEqual(["KeyA↓", "KeyA↑"]);
  });

  test("a key held goes down, and up when the finger lifts", () => {
    input.keyTouch("down", 1, A, 100, 100);
    clock.advance(HOLD_MS);
    expect(log).toEqual(["KeyA↓"]);
    input.keyTouch("up", 1, A, 100, 100);
    expect(log).toEqual(["KeyA↓", "KeyA↑"]);
  });

  test("Ctrl tapped is down for the next key only, and the keys show it", () => {
    tap(CTRL);
    expect(input.state().mods.get(CTRL.code)).toBe("latched");
    tap(C);
    tap(A);
    expect(log).toEqual(["ControlLeft↓", "KeyC↓", "KeyC↑", "ControlLeft↑", "KeyA↓", "KeyA↑"]);
    expect(input.state().mods.size).toBe(0);
  });

  test("Ctrl tapped, then a tap on the trackpad, is a Ctrl-click", () => {
    tap(CTRL);
    padTap();
    expect(log).toEqual(["ControlLeft↓", "BTN_LEFT↓", "BTN_LEFT↑", "ControlLeft↑"]);
  });

  test("Shift tapped twice locks it through several keys", () => {
    tap(SHIFT);
    tap(SHIFT);
    tap(A);
    tap(A);
    expect(input.state().mods.get(SHIFT.code)).toBe("locked");
    expect(log).toEqual(["ShiftLeft↓", "KeyA↓", "KeyA↑", "KeyA↓", "KeyA↑"]);
  });

  test("Caps Lock: the keys show it, and the clone gets its key", () => {
    tap(CAPS);
    expect(input.state().caps).toBe(true);
    tap(CAPS);
    expect(input.state().caps).toBe(false);
    expect(log).toEqual(["CapsLock↓", "CapsLock↑", "CapsLock↓", "CapsLock↑"]);
  });

  test("Fn switches the keys and sends nothing", () => {
    tap(FN);
    expect(input.state().fn).toBe(true);
    tap(FN);
    expect(input.state().fn).toBe(false);
    expect(log).toEqual([]);
  });

  test("Fn+5 copies the clone's clipboard here, and says so for a while", async () => {
    clip = { toLocal: async () => "written", toClone: async () => null };
    tap(TO_LOCAL);
    await settle();
    expect(input.state().clipNote).toBe("Copied the clone's clipboard here.");
    expect(log).toEqual([]);
    clock.advance(CLIP_NOTE_MS);
    expect(input.state().clipNote).toBeNull();
  });

  test("Fn+6 types this device's clipboard into the clone", async () => {
    clip = { toLocal: async () => "same", toClone: async () => ({ text: "h\r\ni" }) };
    tap(TO_CLONE);
    await settle();
    expect(log).toEqual(["'h'↓", "'h'↑", "0xff0d↓", "0xff0d↑", "'i'↓", "'i'↑"]);
    expect(input.state().clipNote).toBe("Typed this clipboard into the clone.");
  });

  test("a clipboard key with no session says so", () => {
    tap(TO_CLONE);
    expect(input.state().clipNote).toBe("Not connected to the clone.");
  });

  test("dictation's key starts it, and its text is typed", () => {
    tap(DICTATE);
    expect(input.state().dictation).toBe("listening");
    dictation.interim("hel");
    expect(input.state().heard).toBe("hel");
    dictation.final("hé");
    expect(log).toEqual(["'h'↓", "'h'↑", "'é'↓", "'é'↑"]);
  });

  test("three fingers that swipe send the overview's and the workspaces' keys", () => {
    swipe(0, -SWIPE_PX * 2);
    swipe(-SWIPE_PX * 2, 0);
    expect(log).toEqual([
      "MetaLeft↓",
      "MetaLeft↑",
      "ControlLeft↓",
      "AltLeft↓",
      "ArrowRight↓",
      "ArrowRight↑",
      "AltLeft↑",
      "ControlLeft↑",
    ]);
  });

  test("a finger on the trackpad moves the cursor, held in the monitor", () => {
    input.padTouch("down", 1, 300, 300);
    clock.advance(1000);
    input.padTouch("move", 1, 310, 300);
    clock.advance(10);
    input.padTouch("move", 1, 2000, 300);
    expect(log[0]).toMatch(/^move 510\.\d+,400$/);
    expect(log[1]).toBe("move 999,400");
  });

  test("two fingers pinch to zoom the view", () => {
    input.padTouch("down", 1, 100, 300);
    input.padTouch("down", 2, 200, 300);
    input.padTouch("move", 2, 300, 300);
    input.padTouch("move", 2, 400, 300);
    expect(zooms).toEqual([1.5]);
    expect(log).toEqual([]);
  });
});

describe("TouchInput on a terminal", () => {
  beforeEach(terminal);

  test("Ctrl tapped, then C, types Ctrl+C; the next key is plain", () => {
    tap(CTRL);
    tap(C);
    tap(C);
    expect(log).toEqual(['"\\u0003"', '"c"']);
  });

  test("Ctrl tapped, then a tap on the trackpad, clicks with Ctrl down", () => {
    tap(CTRL);
    padTap();
    expect(log).toEqual(["mousedown 400,200 ctrl", "mouseup 400,200 ctrl"]);
    padTap();
    expect(log.slice(2)).toEqual(["mousedown 400,200", "mouseup 400,200"]);
  });

  test("Caps Lock makes the letters upper case", () => {
    tap(CAPS);
    tap(A);
    tap(CAPS);
    tap(A);
    expect(input.state().caps).toBe(false);
    expect(log).toEqual(['"A"', '"a"']);
  });

  test("Fn+6 and dictation type their text as it is", async () => {
    clip = { toLocal: async () => "same", toClone: async () => ({ text: "ls\n" }) };
    tap(TO_CLONE);
    await settle();
    tap(DICTATE);
    dictation.final("hi");
    expect(log).toEqual(['"ls\\n"', '"hi"']);
  });

  test("swipes and pinches do nothing", () => {
    swipe(0, -SWIPE_PX * 2);
    input.padTouch("down", 1, 100, 300);
    input.padTouch("down", 2, 200, 300);
    input.padTouch("move", 2, 300, 300);
    input.padTouch("move", 2, 400, 300);
    expect(zooms).toEqual([]);
    expect(log).toEqual([]);
  });
});
