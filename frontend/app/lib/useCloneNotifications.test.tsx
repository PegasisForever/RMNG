// Where a clone's stop is said (`./useCloneNotifications.ts`): in the page while a monitor window
// has the focus, else by the system, and in the page when the system cannot. Needs a DOM:
// `@happy-dom/global-registrator` puts one on the globals before React is imported.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

import type { Clone } from "./types";

if (!GlobalRegistrator.isRegistered) GlobalRegistrator.register();
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
const { act } = await import("react");
const { createRoot } = await import("react-dom/client");
const { MONITOR_CHANNEL } = await import("./clientLayout");
const { useCloneNotifications } = await import("./useCloneNotifications");
type Hook = ReturnType<typeof useCloneNotifications>;

/** The system's notifications that were made, by title. */
let made: string[] = [];
class FakeNotification {
  static permission: NotificationPermission = "granted";
  static requestPermission = async () => FakeNotification.permission;
  onclick: (() => void) | null = null;
  constructor(title: string) {
    made.push(title);
  }
  close() {}
}

let focused = false;
let others: BroadcastChannel[] = [];
let root: ReturnType<typeof createRoot> | null = null;
let hook: Hook | null = null;

beforeEach(() => {
  made = [];
  focused = false;
  FakeNotification.permission = "granted";
  (globalThis as { Notification?: unknown }).Notification = FakeNotification;
  document.hasFocus = () => focused;
});

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  for (const ch of others) ch.close();
  others = [];
});

/** Another monitor window that has the focus: it answers on the monitor channel. */
function focusedOtherWindow() {
  const ch = new BroadcastChannel(MONITOR_CHANNEL);
  ch.onmessage = (e: MessageEvent<{ t: string }>) => {
    if (e.data.t === "ask-focus") ch.postMessage({ t: "focus", monitor: 1 });
  };
  others.push(ch);
}

const clone = (unread: boolean, over: Partial<Clone> = {}): Clone =>
  ({ id: "pega-we-1", host: "pega-we-1", managed: true, displayName: "Fix it", monitorState: "idle", unread, ...over }) as Clone;

function Probe({ hosts }: { hosts: Clone[] }) {
  hook = useCloneNotifications(hosts, [], () => {}, true);
  return null;
}

function show(hosts: Clone[]) {
  if (!root) root = createRoot(document.createElement("div"));
  act(() => root!.render(<Probe hosts={hosts} />));
}

/** The clone stops working, and the windows get the time to answer. */
async function stop(over: Partial<Clone> = {}) {
  show([clone(false, over)]);
  show([clone(true, over)]);
  await act(() => new Promise((r) => setTimeout(r, 400)));
}

describe("a clone that stops working", () => {
  test("is an alert in the page while this window has the focus", async () => {
    focused = true;
    await stop();
    expect(hook!.alerts.map((a) => a.name)).toEqual(["Fix it"]);
    expect(made).toEqual([]);
  });

  test("is an alert in the page while another monitor window has the focus", async () => {
    focusedOtherWindow();
    await stop();
    expect(hook!.alerts).toHaveLength(1);
    expect(made).toEqual([]);
  });

  test("is the system's notification while no monitor window has the focus", async () => {
    await stop({ monitorState: "offline" });
    expect(made).toEqual(["Fix it stopped working"]);
    expect(hook!.alerts).toEqual([]);
  });

  test("is an alert in the page when the system may not notify", async () => {
    FakeNotification.permission = "denied";
    await stop();
    expect(made).toEqual([]);
    expect(hook!.alerts).toHaveLength(1);
  });

  test("its alert goes when the clone is no longer unread", async () => {
    focused = true;
    await stop();
    show([clone(false)]);
    expect(hook!.alerts).toEqual([]);
  });

  test("its alert goes on a click", async () => {
    focused = true;
    await stop();
    act(() => hook!.open("pega-we-1"));
    expect(hook!.alerts).toEqual([]);
  });

  test("a clone seen unread on load says nothing", async () => {
    focused = true;
    show([clone(true)]);
    await act(() => new Promise((r) => setTimeout(r, 400)));
    expect(hook!.alerts).toEqual([]);
    expect(made).toEqual([]);
  });
});
