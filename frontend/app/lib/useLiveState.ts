// The live server state the app draws from.
import { useEffect, useState } from "react";

import { openLiveEvents } from "~/lib/liveEvents";
import { emptyState } from "~/lib/types";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { LxcStats } from "~/lib/wire/LxcStats";

// How long to wait before reloading onto a new server build. Long enough for the notes
// editor's 600ms autosave debounce to fire, short enough that nobody reads a stale page.
const RELOAD_DELAY_MS = 1_500;

/** The server's state, empty until the first frame, kept live by the SSE stream
 *  (`openLiveEvents`, which also rebuilds it after a network drop or a sleep). The same
 *  connection carries persisted `ControlState` plus volatile clone (`stats`) and CT-wide
 *  (`lxcStats`) resource events; neither metric stream touches `state.json`. */
export function useLiveState() {
  const [state, setState] = useState(emptyState);
  const [stats, setStats] = useState<Record<string, ContainerStats>>({});
  const [lxcStats, setLxcStats] = useState<LxcStats | null>(null);
  useEffect(() => {
    // The build this page belongs to, learned from the first `version` frame. The server
    // sends one per connection, and an upgrade drops every connection, so the reconnect
    // after an update is where a different id shows up.
    let buildId: string | null = null;
    // Parse a frame and hand it on; a malformed frame is ignored.
    const json =
      <T,>(set: (v: T) => void) =>
      (data: string) => {
        try {
          set(JSON.parse(data));
        } catch {
          // ignore malformed frame
        }
      };
    const close = openLiveEvents("/events", {
      message: json(setState),
      stats: json(setStats),
      lxcStats: json(setLxcStats),
      // The server restarted onto a different build, so this page's bundle is stale: its
      // JavaScript may be calling routes that moved or reading fields that changed shape.
      // Reload rather than let it keep talking to a server it wasn't built against.
      version: (data) => {
        let next: string | null = null;
        try {
          next = (JSON.parse(data) as { buildId?: string }).buildId ?? null;
        } catch {
          return; // malformed frame — leave the page alone
        }
        if (!next) return;
        if (buildId === null) {
          buildId = next;
          return;
        }
        if (next !== buildId) {
          close(); // stop the watchdog rebuilding the socket under a reloading page
          window.setTimeout(() => window.location.reload(), RELOAD_DELAY_MS);
        }
      },
    });
    return close;
  }, []);
  return { state, stats, lxcStats };
}
