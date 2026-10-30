// The Edit layout dialog's container: the draft of this browser's layout, the sizes of its open
// viewer windows (asked on `VIEWER_CHANNEL`), and the save, after which the layout is in force
// at once (`onSaved`: the dashboard claims it).
import { useState } from "react";

import { LayoutEditorView } from "~/components/LayoutEditorView";
import {
  DEFAULT_DESKTOP_LAYOUT,
  type DesktopLayout,
  VIEWER_CHANNEL,
  type ViewerChannelMsg,
  readDesktopLayout,
  saveDesktopLayout,
} from "~/lib/clientLayout";
import { resize } from "~/lib/monitorLayout";

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
  onSaved,
  onClose,
}: {
  onSaved: (layout: DesktopLayout) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<DesktopLayout>(() => readDesktopLayout() ?? DEFAULT_DESKTOP_LAYOUT);
  const [matching, setMatching] = useState(false);
  const [matchNote, setMatchNote] = useState<string | null>(null);

  const matchWindows = async () => {
    setMatching(true);
    const sizes = await askWindowSizes();
    setMatching(false);
    setDraft((d) => {
      const s = d.pixelScale;
      const next = d.monitors.map((m, i) => {
        const w = sizes.get(i);
        return w ? { width: w.width / s, height: w.height / s } : m;
      });
      return { ...d, monitors: resize(d.monitors, next) };
    });
    const missing = draft.monitors.map((_, i) => i).filter((i) => !sizes.has(i));
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
      layout={draft}
      onChange={setDraft}
      matchNote={matchNote}
      matching={matching}
      onMatchWindows={matchWindows}
      onSave={() => {
        saveDesktopLayout(draft);
        onSaved(draft);
      }}
      onClose={onClose}
    />
  );
}
