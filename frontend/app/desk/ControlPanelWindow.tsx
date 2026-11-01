// The control panel in a window of its own (`/app?control-panel`), opened from the control panel's tool row on a
// desktop (`./FullDesk.tsx`): the tool row and the control panel (or, wide, its columns) fill
// the window, on a solid ground. It shows no clone and claims no layout. Choosing a clone puts
// it on the screens and keeps the page here.
import { useState } from "react";

import { useLiveState } from "~/lib/useLiveState";

import { formatLxcUsage } from "~/lib/format";

import { ControlPanelContainer } from "./control/ControlPanelContainer";
import type { PanelTab } from "./ToolRow";

export function ControlPanelWindow({ width }: { width: number; height: number }) {
  const live = useLiveState();
  const [tab, setTab] = useState<PanelTab>("clones");
  const unread = live.state.hosts.filter((h) => h.unread && !h.archived && h.id !== live.state.selected).length;
  return (
    // No clone's screen under them here: a solid ground in the page's look, no glass
    <div className="absolute inset-0 bg-neutral-100 dark:bg-neutral-950">
      <ControlPanelContainer
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
