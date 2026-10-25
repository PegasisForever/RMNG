import { beforeEach, describe, expect, test } from "bun:test";

import { BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, type PadOut, TAP_WAIT_MS, Trackpad } from "./trackpad";

/** A timer run by hand: `run()` fires what is due by `now`. */
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

let out: PadOut[];
let clock: ReturnType<typeof handTimer>;
let pad: Trackpad;
beforeEach(() => {
  out = [];
  clock = handTimer();
  pad = new Trackpad((o) => out.push(o), clock.timer);
});

const click = (button: number): PadOut[] => [
  { t: "button", button, pressed: true },
  { t: "button", button, pressed: false },
];
const kinds = () => out.map((o) => (o.t === "button" ? `${o.button === BTN_LEFT ? "L" : o.button === BTN_RIGHT ? "R" : "M"}${o.pressed ? "↓" : "↑"}` : o.t));

describe("Trackpad", () => {
  test("one finger moves the cursor, past a small slop", () => {
    pad.down(1, 100, 100, 0);
    pad.move(1, 102, 100, 10);
    expect(out).toEqual([]);
    pad.move(1, 110, 100, 20);
    pad.move(1, 115, 103, 30);
    pad.up(1, 40);
    expect(out).toEqual([
      { t: "move", dx: 10, dy: 0, ms: 20 },
      { t: "move", dx: 5, dy: 3, ms: 10 },
    ]);
    clock.advance(TAP_WAIT_MS);
    expect(out.length).toBe(2); // a move is no tap
  });

  test("a tap clicks once no second touch comes", () => {
    pad.down(1, 100, 100, 0);
    pad.up(1, 80);
    expect(out).toEqual([]);
    clock.advance(TAP_WAIT_MS);
    expect(out).toEqual(click(BTN_LEFT));
  });

  test("a long touch that does not move is no click", () => {
    pad.down(1, 100, 100, 0);
    pad.up(1, 600);
    clock.advance(TAP_WAIT_MS);
    expect(out).toEqual([]);
  });

  test("a tap, then a touch that moves, drags with the left button down", () => {
    pad.down(1, 100, 100, 0);
    pad.up(1, 80);
    clock.advance(100);
    pad.down(2, 100, 100, 180);
    pad.move(2, 130, 100, 200);
    pad.move(2, 160, 100, 220);
    pad.up(2, 400);
    clock.advance(TAP_WAIT_MS);
    expect(kinds()).toEqual(["L↓", "move", "move", "L↑"]);
  });

  test("two taps double-click", () => {
    pad.down(1, 100, 100, 0);
    pad.up(1, 80);
    clock.advance(100);
    pad.down(2, 100, 100, 180);
    pad.up(2, 250);
    expect(out).toEqual([...click(BTN_LEFT), ...click(BTN_LEFT)]);
    clock.advance(TAP_WAIT_MS);
    expect(out.length).toBe(4);
  });

  test("two fingers tap: right click, at once", () => {
    pad.down(1, 100, 100, 0);
    pad.down(2, 150, 100, 10);
    pad.up(1, 100);
    pad.up(2, 110);
    expect(out).toEqual(click(BTN_RIGHT));
  });

  test("three fingers tap: middle click", () => {
    pad.down(1, 100, 100, 0);
    pad.down(2, 150, 100, 5);
    pad.down(3, 200, 100, 10);
    for (const id of [1, 2, 3]) pad.up(id, 100);
    expect(out).toEqual(click(BTN_MIDDLE));
  });

  test("two fingers move: scroll by their mean, then the end, and no click", () => {
    pad.down(1, 100, 100, 0);
    pad.down(2, 150, 100, 0);
    pad.move(1, 100, 80, 20);
    pad.move(2, 150, 80, 20);
    pad.up(1, 300);
    // the last finger moving alone does not move the cursor
    pad.move(2, 150, 40, 320);
    pad.up(2, 340);
    expect(out).toEqual([
      { t: "scroll", dx: 0, dy: -10 },
      { t: "scroll", dx: 0, dy: -10 },
      { t: "scroll_end" },
    ]);
  });

  test("going back to the keys mid-drag lets the button go", () => {
    pad.down(1, 100, 100, 0);
    pad.up(1, 80);
    pad.down(2, 100, 100, 150);
    pad.move(2, 140, 100, 170);
    pad.cancel();
    expect(kinds()).toEqual(["L↓", "move", "L↑"]);
  });

  test("going back to the keys drops a waiting click", () => {
    pad.down(1, 100, 100, 0);
    pad.up(1, 80);
    pad.cancel();
    clock.advance(TAP_WAIT_MS);
    expect(out).toEqual([]);
  });
});
