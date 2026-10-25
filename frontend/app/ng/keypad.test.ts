import { beforeEach, describe, expect, test } from "bun:test";

import { HOLD_MS, type KeypadOut, Keypad } from "./keypad";
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

const said = () =>
  out.map((o) =>
    o.t === "tap" ? `tap ${o.key.id}` : o.t === "hold" ? `${o.key.id}${o.pressed ? "↓" : "↑"}` : o.t,
  );

describe("Keypad", () => {
  test("a finger that lifts without moving taps its key", () => {
    pad.down(1, A, 100, 100, 0);
    pad.move(1, 103, 102, 30); // a fingertip rolls a little
    clock.advance(100);
    pad.up(1);
    expect(said()).toEqual(["tap KeyA"]);
  });

  test("a finger that stays down holds the key until it lifts", () => {
    pad.down(1, A, 100, 100, 0);
    clock.advance(HOLD_MS);
    pad.move(1, 140, 100, 400); // a held key ignores the finger moving
    pad.up(1);
    expect(said()).toEqual(["KeyA↓", "KeyA↑"]);
  });

  test("a modifier is only tapped, also after a long touch", () => {
    pad.down(1, CTRL, 100, 100, 0);
    clock.advance(HOLD_MS * 4);
    pad.up(1);
    expect(said()).toEqual(["tap ControlLeft"]);
  });

  test("a finger that moves moves the cursor and types nothing", () => {
    pad.down(1, A, 100, 100, 0);
    pad.move(1, 120, 100, 20);
    pad.move(1, 130, 105, 30);
    clock.advance(HOLD_MS);
    pad.up(1);
    expect(out).toEqual([
      { t: "move", dx: 20, dy: 0, ms: 20 },
      { t: "move", dx: 10, dy: 5, ms: 10 },
    ]);
  });

  test("two fingers that move scroll by their mean, then end, and type nothing", () => {
    pad.down(1, A, 100, 100, 0);
    pad.down(2, S, 150, 100, 0);
    pad.move(1, 100, 80, 20);
    pad.move(2, 150, 80, 20);
    pad.up(1);
    pad.up(2);
    clock.advance(HOLD_MS);
    expect(out).toEqual([
      { t: "scroll", dx: 0, dy: -10 },
      { t: "scroll", dx: 0, dy: -10 },
      { t: "scroll_end" },
    ]);
  });

  test("fingers that roll from one key to the next are two taps", () => {
    pad.down(1, A, 100, 100, 0);
    clock.advance(60);
    pad.down(2, S, 150, 100, 60);
    pad.up(1);
    clock.advance(60);
    pad.up(2);
    expect(said()).toEqual(["tap KeyA", "tap KeyS"]);
  });

  test("a key held down keeps typing while another finger moves the cursor", () => {
    pad.down(1, A, 100, 100, 0);
    clock.advance(HOLD_MS);
    pad.down(2, S, 200, 100, 300);
    pad.move(2, 230, 100, 320);
    pad.up(2);
    pad.up(1);
    expect(said()).toEqual(["KeyA↓", "move", "KeyA↑"]);
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

