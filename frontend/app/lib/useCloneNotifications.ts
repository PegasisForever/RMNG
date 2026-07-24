import { useEffect, useRef } from "react";
import type { Host } from "./types";

/** Browser/OS notifications for clones that stop working.
 *
 *  We key off the server's `unread` flag rather than diffing `monitorState` ourselves.
 *  The control-server sets `unread = true` exactly when a clone goes `working` →
 *  `idle`/`offline` *while it is not the selected clone* (see `monitor.rs`), and clears
 *  it whenever the clone is selected or starts working again. That already encodes both
 *  the transition detection and the "don't nag about the clone I'm looking at" rule, so a
 *  `false → true` edge on `unread` is precisely one "stopped working" event to surface.
 *
 *  Seeding: a host is only a fresh edge if we've previously seen it *not* unread. A host
 *  seen for the first time (initial SSE frame, or a just-created clone) is baselined
 *  silently, so an already-unread clone never fires retroactively on page load. */
export function useCloneNotifications(hosts: Host[], onActivate?: (id: string) => void) {
  const seen = useRef<Map<string, boolean>>(new Map());
  // Latest-ref for the activate callback so a notification's click handler (created in an
  // effect) always calls the current one, without re-running the effect on every render.
  const activateRef = useRef(onActivate);
  activateRef.current = onActivate;

  // Ask once on mount. The browser remembers the grant/deny per origin, so this is a
  // no-op after the first answer.
  useEffect(() => {
    if (typeof Notification !== "undefined" && Notification.permission === "default") {
      Notification.requestPermission().catch(() => {});
    }
  }, []);

  useEffect(() => {
    const prev = seen.current;
    seen.current = new Map(hosts.map((h) => [h.id, !!h.unread]));

    if (typeof Notification === "undefined" || Notification.permission !== "granted") return;

    for (const h of hosts) {
      if (h.unread && prev.has(h.id) && !prev.get(h.id)) {
        notifyStopped(h, activateRef.current);
      }
    }
  }, [hosts]);
}

function notifyStopped(host: Host, onActivate?: (id: string) => void) {
  const name = host.displayName ?? host.id;
  const offline = host.monitorState === "offline";
  try {
    const n = new Notification(`${name} stopped working`, {
      body: offline ? "The clone went offline." : "The clone is now idle.",
      // One notification per clone: a repeat edge replaces the prior card rather than stacking.
      tag: `clone-stopped-${host.id}`,
    });
    // Click → focus this tab and select the clone that stopped.
    n.onclick = () => {
      window.focus();
      onActivate?.(host.id);
      n.close();
    };
  } catch {
    // Constructing a Notification throws on some platforms (e.g. Android Chrome wants a
    // service worker). A dropped notification is non-fatal.
  }
}
