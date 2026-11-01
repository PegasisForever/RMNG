// The Edit layout dialog's container: this browser's layout, each change saved and in force at
// once (`onApplied`: the page claims it); the monitor windows (the viewer's, or `/ng`'s), one
// opened for a new monitor, the others told their new numbers, and all told to show their
// numbers while the dialog is open (on `VIEWER_CHANNEL`); and their sizes, asked there too.
import { useEffect, useState } from "react";

import { type LayoutChange, LayoutEditorView } from "~/components/LayoutEditorView";
import {
  DEFAULT_DESKTOP_LAYOUT,
  type BrowserLayout,
  IDENTIFY_EVERY_MS,
  VIEWER_CHANNEL,
  type ViewerChannelMsg,
  readLayout,
  saveLayout,
} from "~/lib/clientLayout";
import { resize } from "~/lib/monitorLayout";
import { askWindowSizes } from "~/lib/useViewerChannel";
import { openViewerWindows } from "~/viewer/open";

export function LayoutEditorContainer({
  onApplied,
  onClose,
  page = "/viewer",
  windows = true,
  here,
}: {
  /** The layout changed and is saved: put it in force. */
  onApplied: (layout: BrowserLayout) => void;
  onClose: () => void;
  /** The page each monitor's window shows. */
  page?: string;
  /** The browser can open windows (`canOpenWindows`); else it has one monitor, this page. */
  windows?: boolean;
  /** The monitor of the window the dialog is open in, when it is a monitor's window. */
  here?: number;
}) {
  const [layout, setLayout] = useState<BrowserLayout>(() => readLayout() ?? DEFAULT_DESKTOP_LAYOUT);
  const apply = (next: BrowserLayout, change?: LayoutChange) => {
    setLayout(next);
    saveLayout(next);
    onApplied(next);
    if (change?.t === "add") {
      // In the click itself, so the browser lets the window open
      void openViewerWindows([{ id: change.monitor }], page);
    } else if (change?.t === "renumber") {
      const ch = new BroadcastChannel(VIEWER_CHANNEL);
      const msg: ViewerChannelMsg = { t: "renumber", map: change.map };
      ch.postMessage(msg);
      ch.close();
    }
  };
  /** Give monitor `i` the size of its window, at the pixel scale. */
  const [matching, setMatching] = useState<number | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const matchWindow = async (i: number) => {
    setMatching(i);
    setNote(null);
    const size = (await askWindowSizes()).get(i);
    setMatching(null);
    if (!size) {
      setNote(`No window is open for monitor ${i}. Open it, size it, then try again.`);
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
      here={here}
      onOpenWindow={windows ? (i) => void openViewerWindows([{ id: i }], page) : undefined}
      matching={matching}
      note={note}
      onClose={onClose}
    />
  );
}
