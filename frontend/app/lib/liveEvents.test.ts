// `openLiveEvents` is what brings the app back after a sleep or a network drop. These cases
// drive it with a fake `EventSource`, a fake clock and fake timers.
import { afterEach, beforeEach, describe, expect, setSystemTime, test } from "bun:test";

import {
  CONNECT_MAX_MS,
  CONNECT_MIN_MS,
  PING_MS,
  RETRY_FIRST_MS,
  SSE_STALE_MS,
  WAKE_STALE_MS,
  openLiveEvents,
} from "./liveEvents";

class FakeEventSource {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;
  static all: FakeEventSource[] = [];
  readyState = FakeEventSource.CONNECTING;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
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
  fail() {
    this.readyState = FakeEventSource.CONNECTING;
    this.onerror?.();
  }
  emit(name: string, data = "{}") {
    this.listeners.get(name)?.({ data });
  }
}

/** Fake timers on the fake clock: intervals and timeouts fire as `advance` moves it. */
type Timer = { id: number; at: number; every: number; fn: () => void };
let timers: Timer[] = [];
let nextId = 1;
let now = 0;
const page = new Map<string, () => void>();
const addTimer = (fn: () => void, ms: number, every: number) => {
  const id = nextId++;
  timers.push({ id, at: now + ms, every, fn });
  return id;
};
const clearTimer = (id?: number) => {
  timers = timers.filter((t) => t.id !== id);
};
/** Move the clock on in 100 ms steps, firing what is due. */
function advance(ms: number) {
  const end = now + ms;
  while (now < end) {
    now = Math.min(now + 100, end);
    setSystemTime(now);
    for (const t of [...timers].sort((a, b) => a.at - b.at)) {
      if (t.at > now || !timers.includes(t)) continue;
      if (t.every) t.at += t.every;
      else clearTimer(t.id);
      t.fn();
    }
  }
}
/** The machine sleeps `ms`: the clock jumps, no timer fires on the way. */
function sleep(ms: number) {
  now += ms;
  setSystemTime(now);
  for (const t of timers) if (t.every) t.at = now + t.every;
}

const sockets = () => FakeEventSource.all;
const latest = () => sockets()[sockets().length - 1];

// The globals these tests stub, put back after each: test files share one process, and a later
// file's DOM (happy-dom's) must not find these stubs
const realGlobals = {
  EventSource: globalThis.EventSource,
  window: globalThis.window,
  document: globalThis.document,
};
beforeEach(() => {
  FakeEventSource.all = [];
  timers = [];
  page.clear();
  now = 1_000_000;
  setSystemTime(now);
  const listen = (name: string, fn: () => void) => page.set(name, fn);
  Object.assign(globalThis, {
    EventSource: FakeEventSource,
    window: {
      setInterval: (fn: () => void, ms: number) => addTimer(fn, ms, ms),
      clearInterval: clearTimer,
      setTimeout: (fn: () => void, ms: number) => addTimer(fn, ms, 0),
      clearTimeout: clearTimer,
      addEventListener: listen,
      removeEventListener: (name: string) => page.delete(name),
    },
    document: {
      visibilityState: "visible",
      addEventListener: listen,
      removeEventListener: (name: string) => page.delete(name),
    },
  });
});
afterEach(() => {
  setSystemTime();
  Object.assign(globalThis, realGlobals);
});

describe("openLiveEvents", () => {
  test("pings keep a quiet stream", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    for (let i = 0; i < 20; i++) {
      advance(PING_MS);
      latest().emit("ping");
      page.get("focus")?.();
    }
    expect(sockets()).toHaveLength(1);
    close();
  });

  test("a silent OPEN stream is rebuilt (half-open after a drop)", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    advance(SSE_STALE_MS - 1_000);
    expect(sockets()).toHaveLength(1);
    advance(2_000);
    expect(sockets()).toHaveLength(2);
    expect(sockets()[0].readyState).toBe(FakeEventSource.CLOSED);
    close();
  });

  test("a stream that does not open is given up, in 3 times the last open's time", () => {
    const close = openLiveEvents("/events", {});
    // Before any open: the longest limit
    advance(CONNECT_MAX_MS - 500);
    expect(sockets()).toHaveLength(1);
    advance(1_000);
    expect(sockets()).toHaveLength(2);
    // This one opens at once, so the next limit is the shortest
    latest().open();
    latest().fail();
    advance(100);
    expect(sockets()).toHaveLength(3);
    advance(CONNECT_MIN_MS + 300);
    expect(sockets()).toHaveLength(4);
    // Every second try in a row gets twice as long
    advance(CONNECT_MIN_MS + 300);
    expect(sockets()).toHaveLength(4);
    advance(CONNECT_MIN_MS);
    expect(sockets()).toHaveLength(5);
    advance(CONNECT_MIN_MS + 300);
    expect(sockets()).toHaveLength(6);
    close();
  });

  test("an error retries at once, then backs off, and an event resets it", () => {
    const close = openLiveEvents("/events", {});
    latest().fail();
    expect(sockets()[0].readyState).toBe(FakeEventSource.CLOSED);
    advance(100);
    expect(sockets()).toHaveLength(2);
    latest().fail();
    advance(RETRY_FIRST_MS - 100);
    expect(sockets()).toHaveLength(2);
    advance(100);
    expect(sockets()).toHaveLength(3);
    latest().fail();
    advance(RETRY_FIRST_MS);
    expect(sockets()).toHaveLength(3);
    advance(RETRY_FIRST_MS);
    expect(sockets()).toHaveLength(4);
    latest().open();
    latest().emit("ping");
    latest().fail();
    advance(100);
    expect(sockets()).toHaveLength(5);
    close();
  });

  test("Wi-Fi back: the stream is rebuilt at once, once for signs that come together", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    advance(2_000);
    latest().emit("ping");
    page.get("online")?.();
    expect(sockets()).toHaveLength(2);
    page.get("online")?.();
    page.get("resume")?.();
    expect(sockets()).toHaveLength(2);
    close();
  });

  test("a sleep rebuilds the stream at once", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    advance(2_000);
    sleep(60_000);
    advance(300);
    expect(sockets()).toHaveLength(2);
    close();
  });

  test("a page restored from the back-forward cache is a wake, a first show is not", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    advance(2_000);
    const show = page.get("pageshow") as unknown as (e: { persisted: boolean }) => void;
    show({ persisted: false });
    expect(sockets()).toHaveLength(1);
    show({ persisted: true });
    expect(sockets()).toHaveLength(2);
    close();
  });

  test("the focus rebuilds only a stream that missed a ping, and then the next missed ping does", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    advance(PING_MS - 1_000);
    page.get("focus")?.();
    expect(sockets()).toHaveLength(1);
    advance(WAKE_STALE_MS - PING_MS + 1_000 + 300);
    expect(sockets()).toHaveLength(2);
    latest().open();
    // Out of the wake's window, with the pings coming
    for (let i = 0; i < 3; i++) {
      advance(PING_MS);
      latest().emit("ping");
    }
    advance(WAKE_STALE_MS + 1_000);
    expect(sockets()).toHaveLength(2);
    page.get("focus")?.();
    expect(sockets()).toHaveLength(3);
    close();
  });

  test("a wake while waiting to retry tries now", () => {
    const close = openLiveEvents("/events", {});
    latest().open();
    latest().emit("ping");
    latest().fail();
    latest().fail();
    advance(100);
    latest().fail();
    expect(sockets()).toHaveLength(2);
    page.get("focus")?.();
    expect(sockets()).toHaveLength(3);
    close();
  });

  test("handlers get each event's data, and close stops everything", () => {
    const got: string[] = [];
    const close = openLiveEvents("/events", { message: (d) => got.push(d) });
    latest().emit("message", "snapshot");
    expect(got).toEqual(["snapshot"]);
    close();
    expect(timers).toHaveLength(0);
    expect(page.size).toBe(0);
    advance(SSE_STALE_MS * 3);
    expect(sockets()).toHaveLength(1);
  });
});
