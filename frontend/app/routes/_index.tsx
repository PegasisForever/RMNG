// The route. It holds the one piece of state both shells need (the live SSE stream), the
// first-run config gate, and the phone-or-desktop choice. Everything either shell does with
// that state belongs to its own container.
import { useEffect, useState } from "react";

import { DashboardContainer } from "~/components/DashboardContainer";
import { SetupWizardContainer } from "~/components/SetupWizardContainer";
import { MobileDashboardContainer } from "~/components/mobile/MobileDashboardContainer";
import { getConfig } from "~/lib/api";
import { openLiveEvents } from "~/lib/liveEvents";
import { type ControlState, emptyState } from "~/lib/types";
import { useIsMobile } from "~/lib/useIsMobile";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { LxcStats } from "~/lib/wire/LxcStats";

import type { Route } from "./+types/_index";

export function meta() {
  return [{ title: "RMNG" }];
}

// SPA mode (ssr:false): the live EventSource("/events") delivers the initial full
// state on connect, so the loader just seeds an empty state client-side.
export function clientLoader() {
  return emptyState();
}

// How long to wait before reloading onto a new server build. Long enough for the notes
// editor's 600ms autosave debounce to fire, short enough that nobody reads a stale page.
const RELOAD_DELAY_MS = 1_500;

/** Initial state from the SSR loader, kept live by the SSE stream (`openLiveEvents`, which
 *  also rebuilds it after a network drop or a sleep). The same connection carries persisted
 *  `ControlState` plus volatile clone (`stats`) and CT-wide (`lxcStats`) resource events;
 *  neither metric stream touches `state.json`. */
function useLiveState(initial: ControlState) {
  const [state, setState] = useState(initial);
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

export default function Home({ loaderData }: Route.ComponentProps) {
  // The live SSE state powers both the wizard (template-provision progress) and the
  // dashboard, so it lives here at the gate. `stats` is the volatile per-clone usage map.
  const { state, stats, lxcStats } = useLiveState(loaderData);
  // Phone or desktop, decided once here in JavaScript. The two shells share this state and
  // the config gate above, and nothing else — see `useIsMobile`.
  const mobile = useIsMobile();
  // First-run gate: hold the config (null while loading). Render a minimal centered
  // "Loading…" until it resolves so the dashboard never flashes before the wizard
  // decision; render the wizard INSTEAD of the dashboard while setup isn't complete.
  const [cfg, setCfg] = useState<AppConfigRedacted | null>(null);
  const refetchConfig = () => {
    getConfig()
      .then(setCfg)
      .catch(() => setCfg(null));
  };
  useEffect(() => {
    refetchConfig();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!cfg) {
    // Page-level loading belongs in the container, and this route is the container. It stays
    // inline and unstoried on purpose: no props, no variants, one caller, one word — a story
    // of it would show nothing this line does not.
    return (
      <div className="flex h-screen items-center justify-center text-sm text-slate-400 dark:text-slate-500">
        Loading…
      </div>
    );
  }
  if (!cfg.setupComplete) {
    return <SetupWizardContainer initialConfig={cfg} onDone={refetchConfig} />;
  }
  if (mobile) {
    // The phone tree sizes to the viewport here, so its pages can size to their container
    // and a Storybook frame can stand in for the screen.
    return (
      <div className="h-dvh">
        <MobileDashboardContainer
          state={state}
          groups={state.groups}
          presets={cfg.presets}
        />
      </div>
    );
  }
  return (
    <DashboardContainer
      state={state}
      stats={stats}
      lxcStats={lxcStats}
      sshPublicHost=""
      groups={state.groups}
      presets={cfg.presets}
    />
  );
}
