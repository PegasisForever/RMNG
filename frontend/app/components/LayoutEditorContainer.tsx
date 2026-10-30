// The Edit layout dialog's container: this browser's layout, each change saved and in force at
// once (`onApplied`: the dashboard claims it); the viewer windows, one opened for a new monitor
// and the others told their new numbers (on `VIEWER_CHANNEL`); and their sizes, asked there too.
import { useState } from "react";

import { type LayoutChange, LayoutEditorView } from "~/components/LayoutEditorView";
import {
  DEFAULT_DESKTOP_LAYOUT,
  type DesktopLayout,
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
  const [matching, setMatching] = useState(false);
  const [matchNote, setMatchNote] = useState<string | null>(null);

  const matchWindows = async () => {
    setMatching(true);
    const sizes = await askWindowSizes();
    setMatching(false);
    const s = layout.pixelScale;
    const next = layout.monitors.map((m, i) => {
      const w = sizes.get(i);
      return w ? { width: w.width / s, height: w.height / s } : m;
    });
    if (sizes.size) apply({ ...layout, monitors: resize(layout.monitors, next) });
    const missing = layout.monitors.map((_, i) => i).filter((i) => !sizes.has(i));
    setMatchNote(
      sizes.size === 0
        ? "No viewer window is open. Open the viewer windows, size them, then try again."
        : missing.length
          ? `Matched ${sizes.size} monitor(s). No window open for monitor ${missing.join(", ")}: kept its size.`
          : `Matched all ${sizes.size} monitor(s) to their windows.`,
    );
  };

  return (
    <LayoutEditorView
      layout={layout}
      onChange={apply}
      matchNote={matchNote}
      matching={matching}
      onMatchWindows={matchWindows}
      onClose={onClose}
    />
  );
}
