import { useEffect, useRef } from "react";
import { cloneIndex, isMuted, mutedSet } from "./mute";
import type { Clone } from "./types";

/** Browser/OS notifications for clones that stop working.
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
 *  scope ever widens. */
export function useCloneNotifications(
  hosts: Clone[],
  muted: string[] = [],
  onActivate?: (id: string) => void,
) {
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

  // Keyed on the serialized ids rather than the array, because `mutedClones` is a fresh array
  // on every SSE frame and the effect would otherwise re-run for every unrelated state change.
  const mutedKey = JSON.stringify(muted);

  useEffect(() => {
    const prev = seen.current;
    seen.current = new Map(hosts.map((h) => [h.id, !!h.unread]));

    if (typeof Notification === "undefined" || Notification.permission !== "granted") return;

    const silent = mutedSet(JSON.parse(mutedKey) as string[]);
    const byId = cloneIndex(hosts);
    for (const h of hosts) {
      if (h.parent) continue;
      if (isMuted(h, byId, silent)) continue;
      if (h.unread && prev.has(h.id) && !prev.get(h.id)) {
        notifyStopped(h, activateRef.current);
      }
    }
  }, [hosts, mutedKey]);
}

function notifyStopped(clone: Clone, onActivate?: (id: string) => void) {
  const name = clone.displayName ?? clone.id;
  const offline = clone.monitorState === "offline";
  try {
    const n = new Notification(`${name} stopped working`, {
      body: offline ? "The clone went offline." : "The clone is now idle.",
      // One notification per clone: a repeat edge replaces the prior card rather than stacking.
      tag: `clone-stopped-${clone.id}`,
    });
    // Click → focus this tab and select the clone that stopped.
    n.onclick = () => {
      window.focus();
      onActivate?.(clone.id);
      n.close();
    };
  } catch {
    // Constructing a Notification throws on some platforms (e.g. Android Chrome wants a
    // service worker). A dropped notification is non-fatal.
  }
}
