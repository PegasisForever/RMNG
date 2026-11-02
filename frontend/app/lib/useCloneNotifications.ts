import { useCallback, useEffect, useRef, useState } from "react";
import { cloneIndex, isMuted, mutedSet } from "./mute";
import type { Clone } from "./types";
import { anyMonitorFocused } from "./useMonitorChannel";

/** A clone that stopped working, said in the page (`~/desk/CloneAlerts.tsx`). */
export type CloneAlert = {
  /** The clone's id: one alert per clone. */
  id: string;
  name: string;
  /** The clone went offline (else it is idle). */
  offline: boolean;
  /** When it stopped, ms: a new stop of the same clone is a new alert. */
  at: number;
};

/** How long an alert stays in the page, ms. */
export const ALERT_MS = 10_000;

/** The words of the system's notification of a stop. */
export function alertText(a: Pick<CloneAlert, "name" | "offline">): { title: string; body: string } {
  return {
    title: `${a.name} stopped working`,
    body: a.offline ? "The clone went offline." : "The clone is now idle.",
  };
}

/** Notifications for clones that stop working: in the page, or from the system.
 *
 *  Where: while a monitor window of this browser has the focus (this one, or another that says
 *  so on the monitor channel), the operator looks at RMNG, so the stop is an alert in this page
 *  (`alerts`), which shows even where the system holds its notifications back (a window in
 *  fullscreen). Else it is the system's notification. Where the system cannot show one (no
 *  permission, no API), it is an alert in the page too, which stays until the operator sees it.
 *  An alert goes after ALERT_MS (while the operator was there), on a click (which shows the
 *  clone), on its close button, and when its clone is no longer unread (seen elsewhere, or
 *  working again).
 *
 *  We key off the server's `unread` flag rather than diffing `monitorState` ourselves.
 *  The control-server sets `unread = true` exactly when a clone goes `working` →
 *  `idle`/`offline` *while it is not the selected clone* (see `monitor.rs`), and clears
 *  it whenever the clone is selected or starts working again. That already encodes both
 *  the transition detection and the "don't nag about the clone I'm looking at" rule, so a
 *  `false → true` edge on `unread` is precisely one "stopped working" event to surface.
 *
 *  Seeding: a clone is only a fresh edge if we've previously seen it *not* unread. A clone
 *  seen for the first time (initial SSE frame, or a just-created clone) is baselined
 *  silently, so an already-unread clone never fires retroactively on page load.
 *
 *  Scope: top-level clones only. A sub clone is a helper its parent spawned, so it starts and
 *  stops on its own schedule many times per parent task, and one notification per stop buries
 *  the edges that matter. Sub clones are still baselined in `seen`, so promoting one back to
 *  top level (or losing sight of it and seeing it again) does not fire a stale edge.
 *
 *  Muting: `muted` is `ControlState.mutedClones`, and a clone in it raises nothing. A muted
 *  clone is still baselined, so unmuting it does not fire for a stop that happened while it was
 *  silent. The mute covers sub clones too (see `isMuted`), which today changes nothing on its
 *  own — they are already out of scope — and keeps the two rules from disagreeing if that
 *  scope ever widens.
 *
 *  `enabled`: false in all but one of a browser's windows that show the same clones (there is
 *  one per monitor), so one stop raises one notification. The others ask nothing. */
export function useCloneNotifications(
  hosts: Clone[],
  muted: string[],
  onActivate: (id: string) => void,
  enabled: boolean,
): { alerts: CloneAlert[]; open: (id: string) => void; close: (id: string) => void } {
  const seen = useRef<Map<string, boolean>>(new Map());
  // Latest-ref for the activate callback so a notification's click handler (created in an
  // effect) always calls the current one, without re-running the effect on every render.
  const activateRef = useRef(onActivate);
  activateRef.current = onActivate;

  const [alerts, setAlerts] = useState<CloneAlert[]>([]);
  const alertsRef = useRef(alerts);
  alertsRef.current = alerts;
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const close = useCallback((id: string) => {
    clearTimeout(timers.current.get(id));
    timers.current.delete(id);
    setAlerts((list) => (list.some((a) => a.id === id) ? list.filter((a) => a.id !== id) : list));
  }, []);
  /** Show `alert` (in place of its clone's last one); `stay`: no timer. */
  const show = useCallback(
    (alert: CloneAlert, stay: boolean) => {
      clearTimeout(timers.current.get(alert.id));
      timers.current.delete(alert.id);
      if (!stay) timers.current.set(alert.id, setTimeout(() => close(alert.id), ALERT_MS));
      setAlerts((list) => [...list.filter((a) => a.id !== alert.id), alert]);
    },
    [close],
  );
  const open = useCallback(
    (id: string) => {
      close(id);
      activateRef.current(id);
    },
    [close],
  );
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    const t = timers.current;
    return () => {
      live.current = false;
      for (const timer of t.values()) clearTimeout(timer);
      t.clear();
    };
  }, []);

  // Ask once on mount. The browser remembers the grant/deny per origin, so this is a
  // no-op after the first answer.
  useEffect(() => {
    if (enabled && typeof Notification !== "undefined" && Notification.permission === "default") {
      Notification.requestPermission().catch(() => {});
    }
  }, [enabled]);

  // Keyed on the serialized ids rather than the array, because `mutedClones` is a fresh array
  // on every SSE frame and the effect would otherwise re-run for every unrelated state change.
  const mutedKey = JSON.stringify(muted);

  useEffect(() => {
    const prev = seen.current;
    seen.current = new Map(hosts.map((h) => [h.id, !!h.unread]));

    // An alert goes when its clone is no longer unread (or gone), and all go in a window that
    // no longer notifies
    const unread = new Set(enabled ? hosts.filter((h) => h.unread).map((h) => h.id) : []);
    for (const a of alertsRef.current) if (!unread.has(a.id)) close(a.id);

    if (!enabled) return;

    const silent = mutedSet(JSON.parse(mutedKey) as string[]);
    const byId = cloneIndex(hosts);
    for (const h of hosts) {
      if (h.parent) continue;
      if (isMuted(h, byId, silent)) continue;
      if (h.unread && prev.has(h.id) && !prev.get(h.id)) {
        const alert: CloneAlert = {
          id: h.id,
          name: h.displayName ?? h.id,
          offline: h.monitorState === "offline",
          at: Date.now(),
        };
        void anyMonitorFocused().then((focused) => {
          // Seen, or gone, while the windows answered
          if (!live.current || !seen.current.get(alert.id)) return;
          if (focused) show(alert, false);
          else if (!notifySystem(alert, (id) => activateRef.current(id))) show(alert, true);
        });
      }
    }
  }, [hosts, mutedKey, enabled, close, show]);

  return { alerts, open, close };
}

/** The system's notification of a stop; false: the system cannot show one. */
function notifySystem(alert: CloneAlert, onActivate: (id: string) => void): boolean {
  if (typeof Notification === "undefined" || Notification.permission !== "granted") return false;
  const { title, body } = alertText(alert);
  try {
    const n = new Notification(title, {
      body,
      // One notification per clone: a repeat edge replaces the prior card rather than stacking.
      tag: `clone-stopped-${alert.id}`,
    });
    // Click → select the clone that stopped (`onActivate` brings this window to the front, with
    // the keys in the page).
    n.onclick = () => {
      onActivate(alert.id);
      n.close();
    };
    return true;
  } catch {
    // Constructing a Notification throws on some platforms (e.g. Android Chrome wants a
    // service worker): the page says it instead.
    return false;
  }
}
