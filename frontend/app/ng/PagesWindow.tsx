// The /ng pages in a window of their own (`/ng?pages`), opened from the pages' tool row on a
// desktop (`./FullDesk.tsx`): the tool row and the pages fill the window. It shows no clone and
// claims no layout. Choosing a clone puts it on the screens and keeps the page here.
import { useRef, useState } from "react";

import { emptyState } from "~/lib/types";
import { useLiveState } from "~/lib/useLiveState";

import { formatLxcUsage } from "~/components/BoardRail";

import { GAP, TOOL_H } from "./geometry";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { type NgTab, ToolRow } from "./ToolRow";

/** The room around the tool row and the pages, CSS px. */
const PAD = 4;

export function PagesWindow({ width, height }: { width: number; height: number }) {
  const live = useLiveState(useRef(emptyState()).current);
  const [tab, setTab] = useState<NgTab>("clones");
  const unread = live.state.hosts.filter((h) => h.unread && !h.archived && h.id !== live.state.selected).length;
  return (
    <div className="absolute inset-0 bg-black">
      <ToolRow
        box={{ left: PAD, top: PAD, width: width - 2 * PAD, height: TOOL_H }}
        usage={formatLxcUsage(live.lxcStats)}
        tab={tab}
        onTab={setTab}
        badges={unread ? { clones: unread } : undefined}
      />
      <div
        className="absolute"
        style={{ left: PAD, top: PAD + TOOL_H + GAP, width: width - 2 * PAD, height: height - 2 * PAD - TOOL_H - GAP }}
      >
        <NgPagesContainer
          tab={tab}
          onTab={(t) => t && setTab(t)}
          state={live.state}
          stats={live.stats}
          sound={null}
        />
      </div>
    </div>
  );
}
