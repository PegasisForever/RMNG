import { beforeEach, describe, expect, test } from "bun:test";

import {
  ARROW_STEP_X,
  ARROW_STEP_Y,
  HOLD_MS,
  type KeypadOut,
  Keypad,
  PAD_LINGER_MS,
  PINCH_PX,
  SLOP_PX,
  SWIPE_PX,
  TAP_MS,
  TAP_SLOP_PX,
  TAP_WAIT_MS,
  TWO_SLOP_PX,
} from "./keypad";
import { keyRows } from "./keyboard";

const find = (id: string) => keyRows(false).flat().find((k) => k.id === id)!;
const A = find("KeyA");
const S = find("KeyS");
const CTRL = find("ControlLeft");
const SPACE_KEY = find("Space");

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

  describe("a drag on Space", () => {
    test("types an arrow key for each step sideways or up and down, and the keys stay keys", () => {
      pad.down(1, SPACE_KEY, 100, 100, 0);
      pad.move(1, 100 + SLOP_PX, 100, 50); // the first arrow comes as the drag starts
      pad.move(1, 100 + 3 * ARROW_STEP_X + 1, 100, 100);
      pad.move(1, 100 + 2 * ARROW_STEP_X, 100 - ARROW_STEP_Y, 150); // back one, and up one
      pad.up(1, 200);
      expect(said()).toEqual(["tap ArrowRight", "tap ArrowRight", "tap ArrowRight", "tap ArrowLeft", "tap ArrowUp"]);
      expect(out.some((o) => o.t === "pad" || o.t === "move")).toBe(false);
    });

    test("held still, Space is not held down; it types one space when the finger lifts", () => {
      pad.down(1, SPACE_KEY, 100, 100, 0);
      clock.advance(HOLD_MS * 4);
      pad.up(1, 1000);
      expect(said()).toEqual(["tap Space"]);
    });

    test("in trackpad mode, a drag that starts on Space moves the cursor as anywhere else", () => {
      pad.down(9, A, 100, 100, 0);
      pad.move(9, 160, 100, 20);
      pad.up(9, 40);
      out = [];
      pad.down(1, SPACE_KEY, 100, 100, 100);
      pad.move(1, 160, 100, 150);
      expect(said()).toEqual(["move"]);
    });
  });

  test("a finger that moves less than the threshold still types its key", () => {
    pad.down(1, A, 100, 100, 0);
    pad.move(1, 100 + SLOP_PX - 1, 100, 50);
    pad.up(1, 100);
    expect(said()).toEqual(["tap KeyA"]);
  });

  describe("trackpad mode has no move threshold", () => {
    /** One finger moves the cursor and lifts: the keys are a trackpad. */
    const point = () => {
      pad.down(9, A, 100, 100, 0);
      pad.move(9, 160, 100, 20);
      pad.up(9, 40);
      out = [];
    };
    const moves = () => out.flatMap((o) => (o.t === "move" ? [o.dx] : []));
    const clicks = () => out.filter((o) => o.t === "click").length;

    test("a 2 px move moves the cursor at once", () => {
      point();
      pad.down(1, S, 100, 100, 100);
      pad.move(1, 102, 100, 110);
      expect(moves()).toEqual([2]);
    });

    test("a quick tap that moves a little still clicks", () => {
      point();
      pad.down(1, S, 100, 100, 100);
      pad.move(1, 100 + TAP_SLOP_PX - 2, 100, 150);
      pad.up(1, 100 + TAP_MS - 10);
      clock.advance(TAP_WAIT_MS);
      expect(clicks()).toBe(1);
    });

    test("a touch that moves farther is no click", () => {
      point();
      pad.down(1, S, 100, 100, 100);
      pad.move(1, 100 + TAP_SLOP_PX + 5, 100, 150);
      pad.up(1, 200);
      clock.advance(TAP_WAIT_MS);
      expect(clicks()).toBe(0);
    });

    test("after a tap, a second tap that moves a little double-clicks; one that moves farther drags", () => {
      point();
      pad.down(1, S, 100, 100, 100);
      pad.up(1, 150);
      pad.down(2, S, 100, 100, 200);
      pad.move(2, 103, 100, 220);
      pad.up(2, 260);
      expect(said().filter((x) => x !== "move")).toEqual(["click", "click"]);
      out = [];
      pad.down(3, S, 100, 100, 400);
      pad.up(3, 450);
      pad.down(4, S, 100, 100, 500);
      pad.move(4, 100 + TAP_SLOP_PX, 100, 520);
      pad.up(4, 900);
      expect(out.filter((o) => o.t === "drag").map((o) => o.t === "drag" && o.pressed)).toEqual([true, false]);
    });
  });

  describe("three-finger swipes", () => {
    const swipes = () => out.flatMap((o) => (o.t === "swipe" ? [o.dir] : []));
    /** Three fingers down on the keys, then all moved by (dx, dy) in `steps` steps. */
    const three = (dx: number, dy: number, steps = 4) => {
      const at = [100, 160, 220];
      at.forEach((x, i) => pad.down(i + 1, S, x, 100, 0));
      for (let s = 1; s <= steps; s++) {
        at.forEach((x, i) => pad.move(i + 1, x + (dx * s) / steps, 100 + (dy * s) / steps, s * 20));
      }
      at.forEach((_, i) => pad.up(i + 1, 500));
    };

    test("each way makes its gesture, once per swipe, and types nothing", () => {
      for (const [dx, dy] of [[0, -60], [0, 60], [-60, 0], [60, 0]]) three(dx, dy);
      expect(swipes()).toEqual(["up", "down", "left", "right"]);
      expect(out.some((o) => o.t === "tap" || o.t === "hold" || o.t === "scroll")).toBe(false);
    });

    test("a long swipe is still one gesture", () => {
      three(0, -SWIPE_PX * 6, 12);
      expect(swipes()).toEqual(["up"]);
    });

    test("less than the swipe distance makes none", () => {
      three(0, -(SWIPE_PX - 5) - SLOP_PX * 0);
      expect(swipes()).toEqual([]);
    });

    test("two fingers still scroll; a third that joins ends the scroll and swipes", () => {
      pad.down(1, S, 100, 100, 0);
      pad.down(2, S, 160, 100, 0);
      pad.move(1, 100, 130, 20);
      pad.move(2, 160, 130, 20);
      expect(out.some((o) => o.t === "scroll")).toBe(true);
      pad.down(3, S, 220, 100, 40);
      // The third one's move makes it a swipe; from there on the three go right together
      pad.move(3, 220 + SLOP_PX, 100, 60);
      expect(out.filter((o) => o.t === "scroll_end").length).toBe(1);
      for (const [id, x, y] of [[1, 100, 130], [2, 160, 130], [3, 220 + SLOP_PX, 100]] as const) {
        pad.move(id, x + 2 * SWIPE_PX, y, 80);
      }
      expect(swipes()).toEqual(["right"]);
    });
  });

  describe("two- and three-finger taps", () => {
    const clicks = () => out.flatMap((o) => (o.t === "click" ? [o.button ?? "left"] : []));
    /** Fingers at `xs` down together, then up `ms` later, the first moved `move` px. */
    const tapWith = (xs: number[], ms = 100, move = 0) => {
      xs.forEach((x, i) => pad.down(i + 1, null, x, 100, 1000));
      if (move) pad.move(1, xs[0] + move, 100, 1000 + ms / 2);
      xs.forEach((_, i) => pad.up(i + 1, 1000 + ms));
      clock.advance(TAP_WAIT_MS);
    };
    beforeEach(() => {
      pad = new Keypad((o) => out.push(o), clock.timer, { alwaysPad: true });
    });

    test("two fingers tapped together are one right click; three, a middle click", () => {
      tapWith([100, 160]);
      tapWith([100, 160, 220]);
      expect(clicks()).toEqual(["right", "middle"]);
    });

    test("a slow two-finger touch, or one that moves, is no click", () => {
      tapWith([100, 160], TAP_MS + 50);
      tapWith([100, 160], 100, TAP_SLOP_PX + 5);
      expect(clicks()).toEqual([]);
    });

    test("one finger still left-clicks", () => {
      tapWith([100]);
      expect(clicks()).toEqual(["left"]);
    });
  });

  describe("trackpad only (alwaysPad)", () => {
    beforeEach(() => {
      pad = new Keypad((o) => out.push(o), clock.timer, { alwaysPad: true });
    });

    test("it is a trackpad from the start and stays one: the first touch moves the cursor at once", () => {
      pad.down(1, null, 100, 100, 0);
      pad.move(1, 103, 100, 10);
      pad.up(1, 300);
      clock.advance(PAD_LINGER_MS * 5);
      pad.down(2, null, 100, 100, 6000);
      pad.move(2, 102, 100, 6010);
      expect(out.filter((o) => o.t === "move").map((o) => o.t === "move" && o.dx)).toEqual([3, 2]);
      expect(out.some((o) => o.t === "pad")).toBe(false);
    });
  });

  describe("pinch (the full-screen trackpad)", () => {
    beforeEach(() => {
      pad = new Keypad((o) => out.push(o), clock.timer, { alwaysPad: true, pinch: true });
    });
    const zooms = () => out.flatMap((o) => (o.t === "zoom" ? [o.factor] : []));

    test("two fingers that spread zoom in by the change of their distance, and send no scroll", () => {
      pad.down(1, null, 100, 100, 0);
      pad.down(2, null, 200, 100, 0);
      pad.move(1, 90, 100, 20); // 110 apart: where the pair begins
      pad.move(2, 210, 100, 20); // 120 apart: under PINCH_PX yet
      pad.move(2, 230, 100, 30); // 140 apart: a pinch, x140/110
      pad.move(2, 300, 100, 40); // 210 apart: x1.5 more
      pad.up(1, 100);
      pad.up(2, 100);
      const z = zooms();
      expect(z.length).toBe(2);
      expect(z[0]).toBeCloseTo(140 / 110, 5);
      expect(z[1]).toBeCloseTo(1.5, 5);
      expect(out.some((o) => o.t === "scroll" || o.t === "move")).toBe(false);
    });

    test("two fingers that move together scroll, and do not zoom", () => {
      pad.down(1, null, 100, 100, 0);
      pad.down(2, null, 200, 100, 0);
      for (let i = 1; i <= 5; i++) {
        pad.move(1, 100, 100 + i * 4, i * 10);
        pad.move(2, 200, 100 + i * 4, i * 10);
      }
      expect(zooms()).toEqual([]);
      expect(out.some((o) => o.t === "scroll")).toBe(true);
    });

    test("the thresholds: less than PINCH_PX of spread and TWO_SLOP_PX of travel is neither", () => {
      pad.down(1, null, 100, 100, 0);
      pad.down(2, null, 200, 100, 0);
      pad.move(2, 200 + PINCH_PX - 4, 100 + TWO_SLOP_PX - 2, 10);
      expect(out.some((o) => o.t === "zoom" || o.t === "scroll")).toBe(false);
    });
  });
});
