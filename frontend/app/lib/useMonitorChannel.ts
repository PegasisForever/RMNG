// MONITOR_CHANNEL (`./clientLayout.ts`). A monitor window's side: the monitor windows answer
// the Edit layout dialog with their size, follow their monitor when the monitors are
// renumbered, and show their monitor's number while the dialog is open. The asking side: the
// windows' sizes, and which monitors have a window open.
import { useEffect, useRef, useState } from "react";

import { IDENTIFY_EVERY_MS, MONITOR_CHANNEL, type MonitorChannelMsg } from "./clientLayout";

/** How long the monitor windows get to say their sizes. */
const ANSWER_MS = 400;
/** How often `useOpenMonitors` asks, ms. */
const LOOK_EVERY_MS = 2000;

/** The monitor windows' sizes (this window's too), by monitor, in screen pixels. */
export function askWindowSizes(): Promise<Map<number, { width: number; height: number }>> {
  return new Promise((resolve) => {
    const sizes = new Map<number, { width: number; height: number }>();
    const ch = new BroadcastChannel(MONITOR_CHANNEL);
    ch.onmessage = (e: MessageEvent<MonitorChannelMsg>) => {
      if (e.data.t === "size") sizes.set(e.data.monitor, { width: e.data.width, height: e.data.height });
    };
    const ask: MonitorChannelMsg = { t: "ask-size" };
    ch.postMessage(ask);
    setTimeout(() => {
      ch.close();
      resolve(sizes);
    }, ANSWER_MS);
  });
}

/** The monitors that have a window open in this browser, asked every LOOK_EVERY_MS while
 *  `on` and this window shows, and at once on `look()`; null until the first answer. */
export function useOpenMonitors(on: boolean): { open: ReadonlySet<number> | null; look: () => void } {
  const [open, setOpen] = useState<ReadonlySet<number> | null>(null);
  const lookRef = useRef(() => {});
  useEffect(() => {
    if (!on) {
      setOpen(null);
      return;
    }
    let gone = false;
    const look = () => {
      if (document.visibilityState !== "visible") return;
      void askWindowSizes().then((sizes) => {
        if (!gone) setOpen(new Set(sizes.keys()));
      });
    };
    lookRef.current = look;
    look();
    const timer = setInterval(look, LOOK_EVERY_MS);
    return () => {
      gone = true;
      clearInterval(timer);
      lookRef.current = () => {};
    };
  }, [on]);
  return { open, look: () => lookRef.current() };
}

/** Whether the window should show its monitor's number now (the dialog is open). */
export function useMonitorChannel(
  monitor: number,
  /** The window's room for the picture, in screen (device) pixels. */
  size: () => { width: number; height: number },
  /** The monitor has number `next` now; null: it was removed. */
  onRenumber: (next: number | null) => void,
): boolean {
  const [identify, setIdentify] = useState(false);
  const sizeRef = useRef(size);
  sizeRef.current = size;
  const renumberRef = useRef(onRenumber);
  renumberRef.current = onRenumber;

  useEffect(() => {
    const ch = new BroadcastChannel(MONITOR_CHANNEL);
    let identifyTimer: ReturnType<typeof setTimeout> | undefined;
    ch.onmessage = (e: MessageEvent<MonitorChannelMsg>) => {
      switch (e.data.t) {
        case "renumber": {
          const next = e.data.map[monitor];
          if (next !== undefined && next !== monitor) renumberRef.current(next);
          break;
        }
        case "identify":
          // Shown until the dialog stops saying so
          setIdentify(true);
          clearTimeout(identifyTimer);
          identifyTimer = setTimeout(() => setIdentify(false), IDENTIFY_EVERY_MS * 2.5);
          break;
        case "identify-off":
          clearTimeout(identifyTimer);
          setIdentify(false);
          break;
        case "ask-size": {
          const reply: MonitorChannelMsg = { t: "size", monitor, ...sizeRef.current() };
          ch.postMessage(reply);
          break;
        }
      }
    };
    return () => {
      clearTimeout(identifyTimer);
      ch.close();
    };
  }, [monitor]);

  return identify;
}
