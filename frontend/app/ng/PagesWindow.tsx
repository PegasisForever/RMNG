// The /ng pages in a window of their own (`/ng?pages`), opened from the pages' tool row on a
// desktop (`./FullDesk.tsx`): the tool row and the pages (or, wide, the dashboard's layout) fill
// the window, on a solid ground. It shows no clone and claims no layout. Choosing a clone puts
// it on the screens and keeps the page here.
import { useRef, useState } from "react";

import { emptyState } from "~/lib/types";
import { useLiveState } from "~/lib/useLiveState";

import { formatLxcUsage } from "~/components/BoardRail";

import { NgPagesContainer } from "./pages/NgPagesContainer";
import type { NgTab } from "./ToolRow";

export function PagesWindow({ width }: { width: number; height: number }) {
  const live = useLiveState(useRef(emptyState()).current);
  const [tab, setTab] = useState<NgTab>("clones");
  const unread = live.state.hosts.filter((h) => h.unread && !h.archived && h.id !== live.state.selected).length;
  return (
    // No clone's screen under them here: a solid ground in the page's look, no glass
    <div className="absolute inset-0 bg-neutral-100 dark:bg-neutral-950">
      <NgPagesContainer
        width={width}
        glass={false}
        usage={formatLxcUsage(live.lxcStats)}
        badges={unread ? { clones: unread } : undefined}
        tab={tab}
        onTab={(t) => t && setTab(t)}
        state={live.state}
        stats={live.stats}
        sound={null}
      />
    </div>
  );
}
