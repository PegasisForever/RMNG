// The alerts of clones that stopped working, in the page (`~/lib/useCloneNotifications.ts` says
// when there is one: a monitor window has the focus). In the bottom-right corner of monitor 0's
// window, over the clone's screen (`./ScreenView.tsx`, `alertsRef`): each one is a piece of thick
// glass (`./GLASS.md`) that slides in from the right edge and slides out to it. The newest is at
// the bottom. A click on one shows its clone; its close button only closes it.
import { X } from "lucide-react";
import { useEffect, useState } from "react";

import { UNREAD_DOT } from "~/components/CloneStatus";
import type { CloneAlert } from "~/lib/useCloneNotifications";

import { ICON_BUTTON, PRESS, R_PIECE, TEXT, TEXT_2 } from "./control/ui";

/** How long `app.css` runs an alert's slide out, ms (`rmng-alert-out`). */
export const ALERT_EXIT_MS = 250;

/** The most alerts on screen: the newest ones. */
const MOST = 4;

export function CloneAlerts({
  alerts,
  onOpen,
  onClose,
}: {
  /** The alerts to show, oldest first. */
  alerts: CloneAlert[];
  /** Show the clone (the alert goes). */
  onOpen: (id: string) => void;
  onClose: (id: string) => void;
}) {
  const shown = useLeaving(alerts.slice(-MOST));
  return (
    <div aria-live="polite" className="flex w-80 max-w-full flex-col items-stretch gap-2">
      {shown.map(({ alert, leaving }) => (
        <AlertCard
          key={`${alert.id}:${alert.at}`}
          alert={alert}
          leaving={leaving}
          onOpen={() => onOpen(alert.id)}
          onClose={() => onClose(alert.id)}
        />
      ))}
    </div>
  );
}

function AlertCard({
  alert,
  leaving,
  onOpen,
  onClose,
}: {
  alert: CloneAlert;
  leaving: boolean;
  onOpen: () => void;
  onClose: () => void;
}) {
  return (
    <div
      role="status"
      inert={leaving}
      className={`glass glass-thick pointer-events-auto flex items-start gap-1 rounded-2xl p-1.5 ${
        leaving ? "rmng-alert-out" : "rmng-alert-in"
      }`}
    >
      <button
        type="button"
        onClick={onOpen}
        data-tip="Show this clone"
        className={`flex min-w-0 flex-1 items-start gap-2.5 px-2.5 py-2 text-left ${R_PIECE} ${PRESS}`}
      >
        <span aria-hidden className={`mt-1.5 size-2.5 shrink-0 rounded-full ${UNREAD_DOT}`} />
        <span className="min-w-0">
          {/* The name alone on its line, so a long one that is cut keeps what happened in sight */}
          <span className={`block truncate text-sm font-semibold ${TEXT}`}>{alert.name}</span>
          <span className={`block text-sm ${TEXT_2}`}>
            {alert.offline ? "Stopped working: it went offline." : "Stopped working: it is now idle."}
          </span>
        </span>
      </button>
      <button type="button" aria-label="Close" onClick={onClose} className={`shrink-0 p-1.5 ${ICON_BUTTON}`}>
        <X className="size-4" />
      </button>
    </div>
  );
}

type Shown = { alert: CloneAlert; leaving: boolean };

/** `alerts`, and for ALERT_EXIT_MS the ones that went (`leaving`), in their places. */
function useLeaving(alerts: CloneAlert[]): Shown[] {
  const key = (a: CloneAlert) => `${a.id}:${a.at}`;
  const [shown, setShown] = useState<Shown[]>(() => alerts.map((alert) => ({ alert, leaving: false })));
  const ids = alerts.map(key).join(" ");
  useEffect(() => {
    setShown((prev) => {
      const now = new Set(alerts.map(key));
      const out: Shown[] = [];
      // The ones that went stay where they were, leaving; the new ones come after
      for (const s of prev) out.push(now.has(key(s.alert)) ? { alert: s.alert, leaving: false } : { ...s, leaving: true });
      const had = new Set(prev.map((s) => key(s.alert)));
      for (const alert of alerts) if (!had.has(key(alert))) out.push({ alert, leaving: false });
      return out;
    });
    const t = window.setTimeout(() => setShown((prev) => prev.filter((s) => !s.leaving)), ALERT_EXIT_MS);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ids]);
  return shown;
}
