// The route. It holds the one piece of state both shells need (the live SSE stream), the
// first-run config gate, and the phone-or-desktop choice. Everything either shell does with
// that state belongs to its own container.
import { useEffect, useState } from "react";

import { DashboardContainer } from "~/components/DashboardContainer";
import { SetupWizardContainer } from "~/components/SetupWizardContainer";
import { MobileDashboardContainer } from "~/components/mobile/MobileDashboardContainer";
import { getConfig } from "~/lib/api";
import { type ControlState, emptyState } from "~/lib/types";
import { useIsMobile } from "~/lib/useIsMobile";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";
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

// If no frame or heartbeat has arrived for this long, treat the socket as wedged and
// rebuild it. The server pings every 15s, so ~3 missed pings — comfortably clear of
// jitter but quick enough to recover.
const SSE_STALE_MS = 45_000;

// How long to wait before reloading onto a new server build. Long enough for the notes
// editor's 600ms autosave debounce to fire, short enough that nobody reads a stale page.
const RELOAD_DELAY_MS = 1_500;

/** Initial state from the SSR loader, kept live by the SSE stream. The same connection
 *  carries persisted `ControlState` plus volatile clone (`stats`) and CT-wide (`lxcStats`)
 *  resource events; neither metric stream touches `state.json`.
 *
 *  A plain `EventSource` auto-reconnects when the browser *notices* the socket died — but
 *  after a Wi-Fi drop the TCP connection often goes half-open: it stays `OPEN`, delivers
 *  nothing, and never fires `onerror`, so the UI silently stops updating. We defend against
 *  that with an observable heartbeat + three recovery triggers: a staleness watchdog, the
 *  `online` event (Wi-Fi came back), and tab re-focus. Each rebuilds the connection, and a
 *  fresh `/events` connection replays the full snapshot, so the UI resyncs on reconnect. */
function useLiveState(initial: ControlState) {
  const [state, setState] = useState(initial);
  const [stats, setStats] = useState<Record<string, ContainerStats>>({});
  const [lxcStats, setLxcStats] = useState<LxcStats | null>(null);
  const [forwards, setForwards] = useState<Record<string, ForwardRuntime[]>>({});
  useEffect(() => {
    let es: EventSource | null = null;
    let lastActivity = Date.now();
    let disposed = false; // set on unmount so late callbacks don't reopen
    // The build this page belongs to, learned from the first `version` frame. The server
    // sends one per connection, and an upgrade drops every connection, so the reconnect
    // after an update is where a different id shows up.
    let buildId: string | null = null;

    const connect = () => {
      if (disposed) return;
      es?.close();
      lastActivity = Date.now();
      es = new EventSource("/events");
      es.onopen = () => {
        lastActivity = Date.now();
      };
      es.onmessage = (e) => {
        lastActivity = Date.now();
        try {
          setState(JSON.parse(e.data));
        } catch {
          // ignore malformed frame
        }
      };
      es.addEventListener("stats", (e) => {
        lastActivity = Date.now();
        try {
          setStats(JSON.parse((e as MessageEvent).data));
        } catch {
          // ignore malformed frame
        }
      });
      es.addEventListener("lxcStats", (e) => {
        lastActivity = Date.now();
        try {
          setLxcStats(JSON.parse((e as MessageEvent).data));
        } catch {
          // ignore malformed frame
        }
      });
      es.addEventListener("forwards", (e) => {
        lastActivity = Date.now();
        try {
          setForwards(JSON.parse((e as MessageEvent).data));
        } catch {
          // ignore malformed frame
        }
      });
      // Heartbeat carries no payload — it exists only to keep `lastActivity` fresh so the
      // watchdog can distinguish a wedged socket from an idle-but-healthy one.
      es.addEventListener("ping", () => {
        lastActivity = Date.now();
      });
      // The server restarted onto a different build, so this page's bundle is stale: its
      // JavaScript may be calling routes that moved or reading fields that changed shape.
      // Reload rather than let it keep talking to a server it wasn't built against.
      es.addEventListener("version", (e) => {
        lastActivity = Date.now();
        let next: string | null = null;
        try {
          next = (JSON.parse((e as MessageEvent).data) as { buildId?: string }).buildId ?? null;
        } catch {
          return; // malformed frame — leave the page alone
        }
        if (!next) return;
        if (buildId === null) {
          buildId = next;
          return;
        }
        if (next !== buildId) {
          disposed = true; // stop the watchdog rebuilding the socket under a reloading page
          es?.close();
          window.setTimeout(() => window.location.reload(), RELOAD_DELAY_MS);
        }
      });
    };

    connect();

    // Watchdog: rebuild a socket the browser has given up on (CLOSED), or one that still
    // claims to be OPEN but has gone silent past the staleness window (the half-open case).
    // A CONNECTING socket is the browser's own retry in flight — leave it be.
    const watchdog = window.setInterval(() => {
      if (disposed) return;
      const stale = Date.now() - lastActivity > SSE_STALE_MS;
      if (es?.readyState === EventSource.CLOSED || (es?.readyState === EventSource.OPEN && stale)) {
        connect();
      }
    }, 5_000);

    // Wi-Fi/network regained → rebuild immediately (the current socket is likely half-open).
    const onOnline = () => connect();
    // Tab re-focus after a sleep/background stretch that outran the staleness window.
    const onVisible = () => {
      if (document.visibilityState === "visible" && Date.now() - lastActivity > SSE_STALE_MS) {
        connect();
      }
    };
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      disposed = true;
      window.clearInterval(watchdog);
      window.removeEventListener("online", onOnline);
      document.removeEventListener("visibilitychange", onVisible);
      es?.close();
    };
  }, []);
  return { state, stats, lxcStats, forwards };
}

export default function Home({ loaderData }: Route.ComponentProps) {
  // The live SSE state powers both the wizard (template-provision progress) and the
  // dashboard, so it lives here at the gate. `stats` is the volatile per-clone usage map.
  const { state, stats, lxcStats, forwards } = useLiveState(loaderData);
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
    return (
      <SetupWizardContainer
        operations={state.operations}
        initialConfig={cfg}
        onDone={refetchConfig}
      />
    );
  }
  if (mobile) {
    // The phone tree sizes to the viewport here, so its pages can size to their container
    // and a Storybook frame can stand in for the screen.
    return (
      <div className="h-dvh">
        <MobileDashboardContainer
          state={state}
          cloneGroups={cfg.cloneGroups}
          codexGroups={cfg.codexGroups}
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
      forwards={forwards}
      sshPublicHost={cfg.ssh?.publicHost ?? ""}
      bastionPort={cfg.listen.bastion}
      cloneGroups={cfg.cloneGroups}
      codexGroups={cfg.codexGroups}
      presets={cfg.presets}
    />
  );
}
