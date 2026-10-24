// The watchdog in `openLiveEvents` is what brings the dashboard back after a sleep. These
// cases drive it with a fake `EventSource`, a fake clock and the watchdog's own tick.
import { afterEach, beforeEach, describe, expect, setSystemTime, test } from "bun:test";

import { SSE_STALE_MS, WATCHDOG_MS, openLiveEvents } from "./liveEvents";

class FakeEventSource {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;
  static all: FakeEventSource[] = [];
  readyState = FakeEventSource.CONNECTING;
  onopen: (() => void) | null = null;
  listeners = new Map<string, (e: { data: string }) => void>();
  constructor(public url: string) {
    FakeEventSource.all.push(this);
  }
  addEventListener(name: string, fn: (e: { data: string }) => void) {
    this.listeners.set(name, fn);
  }
  close() {
    this.readyState = FakeEventSource.CLOSED;
  }
  open() {
    this.readyState = FakeEventSource.OPEN;
    this.onopen?.();
  }
  emit(name: string, data = "{}") {
    this.listeners.get(name)?.({ data });
  }
}

let now = 0;
let tick: () => void = () => {};
const sockets = () => FakeEventSource.all;
const latest = () => sockets()[sockets().length - 1];
/** Move the clock on, with the watchdog looking every WATCHDOG_MS on the way. */
function advance(ms: number) {
  for (let t = 0; t < ms; t += WATCHDOG_MS) {
    now += WATCHDOG_MS;
    setSystemTime(now);
    tick();
  }
}

beforeEach(() => {
  FakeEventSource.all = [];
  now = 1_000_000;
  setSystemTime(now);
  Object.assign(globalThis, {
    EventSource: FakeEventSource,
    window: {
      setInterval: (fn: () => void) => ((tick = fn), 1),
      clearInterval: () => (tick = () => {}),
      addEventListener: () => {},
      removeEventListener: () => {},
    },
    document: { addEventListener: () => {}, removeEventListener: () => {} },
  });
});
afterEach(() => setSystemTime());

describe("openLiveEvents watchdog", () => {
  test("pings keep a quiet stream", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    for (let i = 0; i < 10; i++) {
      advance(15_000);
      latest().emit("ping");
    }
    expect(sockets()).toHaveLength(1);
    close();
  });

  test("a silent OPEN stream is rebuilt (half-open after a drop)", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    advance(SSE_STALE_MS + WATCHDOG_MS);
    expect(sockets()).toHaveLength(2);
    expect(sockets()[0].readyState).toBe(FakeEventSource.CLOSED);
    close();
  });

  test("a stream stuck CONNECTING is rebuilt (opened while the network was gone)", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    advance(SSE_STALE_MS + WATCHDOG_MS);
    // The rebuilt one never connects: before the fix, the watchdog left it alone for good.
    expect(latest().readyState).toBe(FakeEventSource.CONNECTING);
    advance(SSE_STALE_MS + WATCHDOG_MS);
    expect(sockets()).toHaveLength(3);
    close();
  });

  test("handlers get each event's data, and close stops the watchdog", () => {
    const got: string[] = [];
    const close = openLiveEvents("/events", { message: (d) => got.push(d) });
    latest().emit("message", "snapshot");
    expect(got).toEqual(["snapshot"]);
    close();
    advance(SSE_STALE_MS * 3);
    expect(sockets()).toHaveLength(1);
  });
});
