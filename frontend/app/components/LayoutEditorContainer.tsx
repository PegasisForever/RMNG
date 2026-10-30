// The Edit layout dialog's container: this browser's layout, each change saved and in force at
// once (`onApplied`: the dashboard claims it); the viewer windows, one opened for a new monitor,
// the others told their new numbers, and all told to show their numbers while the dialog is
// open (on `VIEWER_CHANNEL`); and their sizes, asked there too.
import { useEffect, useState } from "react";

import { type LayoutChange, LayoutEditorView } from "~/components/LayoutEditorView";
import {
  DEFAULT_DESKTOP_LAYOUT,
  type DesktopLayout,
  IDENTIFY_EVERY_MS,
  VIEWER_CHANNEL,
  type ViewerChannelMsg,
  readDesktopLayout,
  saveDesktopLayout,
} from "~/lib/clientLayout";
import { resize } from "~/lib/monitorLayout";
import { openViewerWindows } from "~/viewer/open";

/** How long the viewer windows get to say their sizes. */
const ANSWER_MS = 400;

/** The viewer windows' sizes, by monitor, in screen pixels. */
function askWindowSizes(): Promise<Map<number, { width: number; height: number }>> {
  return new Promise((resolve) => {
    const sizes = new Map<number, { width: number; height: number }>();
    const ch = new BroadcastChannel(VIEWER_CHANNEL);
    ch.onmessage = (e: MessageEvent<ViewerChannelMsg>) => {
      if (e.data.t === "size") sizes.set(e.data.monitor, { width: e.data.width, height: e.data.height });
    };
    const ask: ViewerChannelMsg = { t: "ask-size" };
    ch.postMessage(ask);
    setTimeout(() => {
      ch.close();
      resolve(sizes);
    }, ANSWER_MS);
  });
}

export function LayoutEditorContainer({
  onApplied,
  onClose,
}: {
  /** The layout changed and is saved: put it in force. */
  onApplied: (layout: DesktopLayout) => void;
  onClose: () => void;
}) {
  const [layout, setLayout] = useState<DesktopLayout>(() => readDesktopLayout() ?? DEFAULT_DESKTOP_LAYOUT);
  const apply = (next: DesktopLayout, change?: LayoutChange) => {
    setLayout(next);
    saveDesktopLayout(next);
    onApplied(next);
    if (change?.t === "add") {
      // In the click itself, so the browser lets the window open
      void openViewerWindows([{ id: change.monitor }]);
    } else if (change?.t === "renumber") {
      const ch = new BroadcastChannel(VIEWER_CHANNEL);
      const msg: ViewerChannelMsg = { t: "renumber", map: change.map };
      ch.postMessage(msg);
      ch.close();
    }
  };
  /** Give monitor `i` the size of its viewer window, at the pixel scale. */
  const [matching, setMatching] = useState<number | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const matchWindow = async (i: number) => {
    setMatching(i);
    setNote(null);
    const size = (await askWindowSizes()).get(i);
    setMatching(null);
    if (!size) {
      setNote(`No viewer window is open for monitor ${i}. Open it, size it, then try again.`);
      return;
    }
    const s = layout.scales[i];
    const sizes = layout.monitors.map((m, k) => (k === i ? { width: size.width / s, height: size.height / s } : m));
    apply({ ...layout, monitors: resize(layout.monitors, sizes) });
  };

  // While the dialog is open, each viewer window shows its monitor's number
  useEffect(() => {
    const ch = new BroadcastChannel(VIEWER_CHANNEL);
    const say = (msg: ViewerChannelMsg) => ch.postMessage(msg);
    say({ t: "identify" });
    const timer = setInterval(() => say({ t: "identify" }), IDENTIFY_EVERY_MS);
    return () => {
      clearInterval(timer);
      say({ t: "identify-off" });
      ch.close();
    };
  }, []);

  return (
    <LayoutEditorView
      layout={layout}
      onChange={apply}
      onMatchWindow={matchWindow}
      matching={matching}
      note={note}
      onClose={onClose}
    />
  );
}
