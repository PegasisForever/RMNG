// The route. It holds the one piece of state both shells need (the live SSE stream), the
// first-run config gate, and the phone-or-desktop choice. Everything either shell does with
// that state belongs to its own container.
import { useEffect, useState } from "react";

import { DashboardContainer } from "~/components/DashboardContainer";
import { SetupWizardContainer } from "~/components/SetupWizardContainer";
import { MobileDashboardContainer } from "~/components/mobile/MobileDashboardContainer";
import { getConfig } from "~/lib/api";
import { emptyState } from "~/lib/types";
import { useIsMobile } from "~/lib/useIsMobile";
import { useLiveState } from "~/lib/useLiveState";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

import type { Route } from "./+types/_index";

export function meta() {
  return [{ title: "RMNG" }];
}

// SPA mode (ssr:false): the live EventSource("/events") delivers the initial full
// state on connect, so the loader just seeds an empty state client-side.
export function clientLoader() {
  return emptyState();
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
