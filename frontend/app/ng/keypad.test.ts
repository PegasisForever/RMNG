import { beforeEach, describe, expect, test } from "bun:test";

import { HOLD_MS, type KeypadOut, Keypad, PAD_LINGER_MS, TAP_MS, TAP_WAIT_MS } from "./keypad";
import { keyRows } from "./keyboard";

const find = (id: string) => keyRows(false).flat().find((k) => k.id === id)!;
const A = find("KeyA");
const S = find("KeyS");
const CTRL = find("ControlLeft");

/** A timer run by hand: `advance` fires what is due. */
function handTimer() {
  let jobs: { at: number; fn: () => void }[] = [];
  let now = 0;
  return {
    timer: {
      after(ms: number, fn: () => void) {
        const job = { at: now + ms, fn };
        jobs.push(job);
        return () => {
          jobs = jobs.filter((j) => j !== job);
        };
      },
    },
    advance(ms: number) {
      now += ms;
      const due = jobs.filter((j) => j.at <= now);
      jobs = jobs.filter((j) => j.at > now);
      for (const j of due) j.fn();
    },
  };
}

let out: KeypadOut[];
let clock: ReturnType<typeof handTimer>;
let pad: Keypad;
beforeEach(() => {
  out = [];
  clock = handTimer();
  pad = new Keypad((o) => out.push(o), clock.timer);
});

/** What came out, the trackpad mode changes and the key presses (for the click sound) left
 *  out: their own tests have them. */
const keys = () => out.filter((o) => o.t !== "pad" && o.t !== "press");
const said = () =>
  keys().map((o) =>
    o.t === "tap" ? `tap ${o.key.id}` : o.t === "hold" ? `${o.key.id}${o.pressed ? "↓" : "↑"}` : o.t,
  );

describe("Keypad", () => {
  test("a finger that lifts without moving taps its key", () => {
    pad.down(1, A, 100, 100, 0);
    pad.move(1, 103, 102, 30); // a fingertip rolls a little
    clock.advance(100);
    pad.up(1, 0);
    expect(said()).toEqual(["tap KeyA"]);
  });

  test("a finger that stays down holds the key until it lifts", () => {
    pad.down(1, A, 100, 100, 0);
    clock.advance(HOLD_MS);
    pad.move(1, 140, 100, 400); // a held key ignores the finger moving
    pad.up(1, 0);
    expect(said()).toEqual(["KeyA↓", "KeyA↑"]);
  });

  test("a modifier is told down when the finger comes down and up when it lifts", () => {
    pad.down(1, CTRL, 100, 100, 0);
    clock.advance(HOLD_MS * 4);
    pad.up(1, 0);
    expect(keys()).toEqual([
      { t: "mod", key: CTRL, down: true },
      { t: "mod", key: CTRL, down: false },
    ]);
  });

  test("a touch on a modifier that moves is a drag: the modifier is told up at once", () => {
    pad.down(1, CTRL, 100, 100, 0);
    pad.move(1, 130, 100, 20);
    pad.up(1, 0);
    expect(said()).toEqual(["mod", "mod", "move"]);
    expect(keys()[1]).toEqual({ t: "mod", key: CTRL, down: false, dragged: true });
  });

  test("a finger that moves moves the cursor and types nothing", () => {
    pad.down(1, A, 100, 100, 0);
    pad.move(1, 120, 100, 20);
    pad.move(1, 130, 105, 30);
    clock.advance(HOLD_MS);
    pad.up(1, 0);
    expect(keys()).toEqual([
      { t: "move", dx: 20, dy: 0, ms: 20 },
      { t: "move", dx: 10, dy: 5, ms: 10 },
    ]);
  });

  test("two fingers that move scroll by their mean, then end, and type nothing", () => {
    pad.down(1, A, 100, 100, 0);
    pad.down(2, S, 150, 100, 0);
    pad.move(1, 100, 80, 20);
    pad.move(2, 150, 80, 20);
    pad.up(1, 0);
    pad.up(2, 0);
    clock.advance(HOLD_MS);
    expect(keys()).toEqual([
      { t: "scroll", dx: 0, dy: -10 },
      { t: "scroll", dx: 0, dy: -10 },
      { t: "scroll_end" },
    ]);
  });

  test("fingers that roll from one key to the next are two taps", () => {
    pad.down(1, A, 100, 100, 0);
    clock.advance(60);
    pad.down(2, S, 150, 100, 60);
    pad.up(1, 0);
    clock.advance(60);
    pad.up(2, 0);
    expect(said()).toEqual(["tap KeyA", "tap KeyS"]);
  });

  test("a key held down keeps typing while another finger moves the cursor", () => {
    pad.down(1, A, 100, 100, 0);
    clock.advance(HOLD_MS);
    pad.down(2, S, 200, 100, 300);
    pad.move(2, 230, 100, 320);
    pad.up(2, 0);
    pad.up(1, 0);
    expect(said()).toEqual(["KeyA↓", "move", "KeyA↑"]);
  });

  test("a touch that starts between keys moves the cursor, and never types", () => {
    pad.down(1, null, 100, 100, 0);
    clock.advance(HOLD_MS);
    pad.up(1, 0);
    pad.down(2, null, 100, 100, 400);
    pad.move(2, 120, 100, 420);
    pad.up(2, 0);
    expect(said()).toEqual(["move"]);
  });

  test("a touch the system cancels types nothing and lets a held key go", () => {
    pad.down(1, A, 100, 100, 0);
    pad.cancel(1);
    pad.down(2, S, 100, 100, 0);
    clock.advance(HOLD_MS);
    pad.cancel(2);
    expect(said()).toEqual(["KeyS↓", "KeyS↑"]);
  });
});

describe("Keypad trackpad mode", () => {
  /** A finger drags on the keys, then lifts at `t`. */
  const drag = (id: number, t: number) => {
    pad.down(id, A, 100, 100, t - 50);
    pad.move(id, 130, 100, t - 20);
    pad.up(id, t);
  };
  const pads = () => out.filter((o) => o.t === "pad").map((o) => (o as { on: boolean }).on);

  test("a drag turns the keys into a trackpad until PAD_LINGER_MS after the last finger lifts", () => {
    drag(1, 100);
    expect(pads()).toEqual([true]);
    clock.advance(PAD_LINGER_MS - 1);
    expect(pads()).toEqual([true]);
    clock.advance(1);
    expect(pads()).toEqual([true, false]);
  });

  test("meanwhile a quick tap on a key clicks, once no second touch comes, and starts the wait again", () => {
    drag(1, 100);
    clock.advance(500);
    pad.down(2, A, 100, 100, 600);
    clock.advance(HOLD_MS * 2); // no hold in trackpad mode
    pad.up(2, 600 + TAP_MS - 10);
    expect(said()).toEqual(["move"]);
    clock.advance(TAP_WAIT_MS);
    expect(said()).toEqual(["move", "click"]);
    // the wait for the keys runs from the lift, not from the click
    clock.advance(PAD_LINGER_MS - TAP_WAIT_MS - 1);
    expect(pads()).toEqual([true]);
    clock.advance(1);
    expect(pads()).toEqual([true, false]);
    // keys again: a tap types
    pad.down(3, A, 100, 100, 5000);
    pad.up(3, 5050);
    expect(said().at(-1)).toBe("tap KeyA");
  });

  test("a slow touch in trackpad mode clicks nothing", () => {
    drag(1, 100);
    pad.down(2, A, 100, 100, 200);
    pad.up(2, 200 + TAP_MS + 50);
    expect(said()).toEqual(["move"]);
  });

  test("a touch on a modifier in trackpad mode clicks, and the modifier stays as it was", () => {
    drag(1, 100);
    pad.down(2, CTRL, 100, 100, 200);
    pad.up(2, 250);
    clock.advance(TAP_WAIT_MS);
    expect(said()).toEqual(["move", "click"]);
  });

  test("a tap, then a touch that moves, drags with the left button down until it lifts", () => {
    drag(1, 100);
    pad.down(2, A, 100, 100, 200);
    pad.up(2, 250);
    clock.advance(100);
    pad.down(3, S, 100, 100, 350);
    pad.move(3, 140, 100, 380);
    pad.move(3, 170, 100, 400);
    pad.up(3, 600);
    clock.advance(TAP_WAIT_MS);
    expect(keys().map((o) => (o.t === "drag" ? `drag${o.pressed ? "↓" : "↑"}` : o.t))).toEqual([
      "move",
      "drag↓",
      "move",
      "move",
      "drag↑",
    ]);
  });

  test("two quick taps double-click", () => {
    drag(1, 100);
    pad.down(2, A, 100, 100, 200);
    pad.up(2, 250);
    clock.advance(100);
    pad.down(3, A, 100, 100, 350);
    pad.up(3, 400);
    clock.advance(TAP_WAIT_MS);
    expect(said()).toEqual(["move", "click", "click"]);
  });

  test("the mode holds while a finger is still down", () => {
    pad.down(1, A, 100, 100, 0);
    pad.move(1, 130, 100, 20);
    clock.advance(PAD_LINGER_MS * 3);
    expect(pads()).toEqual([true]);
    pad.up(1, 4000);
    clock.advance(PAD_LINGER_MS);
    expect(pads()).toEqual([true, false]);
  });

  describe("key presses and the mouse buttons", () => {
    const presses = () => out.flatMap((o) => (o.t === "press" ? [o.key.id] : []));
    const modes = () => out.flatMap((o) => (o.t === "pad" ? [o.on] : []));
    /** One finger moves the cursor and lifts: the keys are a trackpad. */
    const point = () => {
      pad.down(9, A, 100, 100, 0);
      pad.move(9, 160, 100, 20);
      pad.up(9, 40);
    };

    test("a finger down on a key presses it at once, also when it turns into a cursor move", () => {
      pad.down(1, A, 100, 100, 0);
      expect(presses()).toEqual(["KeyA"]);
      pad.move(1, 160, 100, 20);
      pad.up(1, 40);
      expect(presses()).toEqual(["KeyA"]);
    });

    test("in trackpad mode and between keys there is no press", () => {
      point();
      out = [];
      pad.down(2, S, 100, 100, 100);
      pad.up(2, 150);
      pad.down(3, null, 10, 10, 2000);
      expect(presses()).toEqual([]);
    });

    test("a mouse button held keeps trackpad mode on; after it lifts the mode lingers, then ends", () => {
      point();
      pad.button(true);
      clock.advance(PAD_LINGER_MS * 3);
      expect(modes()).toEqual([true]);
      pad.button(false);
      clock.advance(PAD_LINGER_MS - 1);
      expect(modes()).toEqual([true]);
      clock.advance(1);
      expect(modes()).toEqual([true, false]);
    });
  });
});
