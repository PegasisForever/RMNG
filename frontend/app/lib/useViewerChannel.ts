// A monitor window's side of VIEWER_CHANNEL (`./clientLayout.ts`): the viewer windows and the
// `/ng` windows answer the Edit layout dialog with their size, follow their monitor when the
// monitors are renumbered, and show their monitor's number while the dialog is open.
import { useEffect, useRef, useState } from "react";

import { IDENTIFY_EVERY_MS, VIEWER_CHANNEL, type ViewerChannelMsg } from "./clientLayout";

/** Whether the window should show its monitor's number now (the dialog is open). */
export function useViewerChannel(
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
    const ch = new BroadcastChannel(VIEWER_CHANNEL);
    let identifyTimer: ReturnType<typeof setTimeout> | undefined;
    ch.onmessage = (e: MessageEvent<ViewerChannelMsg>) => {
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
          const reply: ViewerChannelMsg = { t: "size", monitor, ...sizeRef.current() };
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
