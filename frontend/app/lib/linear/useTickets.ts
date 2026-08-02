// The ticket column's source: every configured Linear key's own open issues, merged, kept
// current by a poll the browser runs itself.
//
// One poll per interval for the whole page, not one per key visible on screen: the query is
// scoped to the key's viewer, and the answer is the same whoever is looking at it. Keys are
// queried in parallel, so a key pointing at a workspace having a bad day costs latency but
// never stalls the others.
//
// A tab that nobody is looking at asks nothing. Linear allows 2,500 requests per user per
// hour, and a minute apart across a handful of tabs is nowhere near that, but a laptop asleep
// in a bag has no reason to spend any of it. The catch-up happens on re-focus.

import { useEffect, useState } from "react";

import { fetchOpenTickets } from "~/lib/linear/queries";
import type { LinearTicket } from "~/lib/linear/types";

/** How often to ask Linear. Fast enough that a ticket someone closes is gone before you
 *  wonder why it is still there. */
const POLL_MS = 60_000;

/** How long to wait after a failure. Linear being down is not urgent, the column keeps
 *  drawing the last good list, so this backs off rather than hammering. */
const ERROR_MS = 5 * 60_000;

export interface Tickets {
  /** Every open issue across the configured keys, deduplicated. Empty until the first answer
   *  lands, and unchanged by a poll in which every key failed. */
  tickets: LinearTicket[];
  /** Why the last poll failed, if it did. Set alongside a partial list when only some keys
   *  answered. */
  error: string | null;
  /** True until the first answer, success or failure, has landed. The column reads it to tell
   *  "still asking" apart from "nothing to show". */
  loading: boolean;
}

/** The ticket column's list, live.
 *
 *  Pass the presets from `GET /api/config`; the keys are read off them and nothing else is.
 *  A preset with no key is skipped, and two presets sharing one key are asked once. */
export function useTickets(presets: { linearKey: string }[]): Tickets {
  const [tickets, setTickets] = useState<LinearTicket[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // The effect restarts when the set of keys changes, which is what picks up a key the
  // operator just typed into Settings. Keyed on the serialized list rather than the array,
  // because `presets` is a fresh array on every render that reaches this hook.
  const keysKey = JSON.stringify(linearKeys(presets));

  useEffect(() => {
    const keys = JSON.parse(keysKey) as string[];
    // Set on unmount, so a fetch still in flight cannot write state into a gone component or
    // arm another timer behind it. Same discipline as the SSE socket's own effect.
    let disposed = false;
    let inFlight = false;
    let timer: number | undefined;

    if (keys.length === 0) {
      setTickets([]);
      setError(null);
      setLoading(false);
      return;
    }

    const arm = (ms: number) => {
      window.clearTimeout(timer);
      timer = window.setTimeout(tick, ms);
    };

    const tick = () => {
      if (disposed || inFlight) return;
      if (document.visibilityState !== "visible") {
        arm(POLL_MS);
        return;
      }
      inFlight = true;
      void poll();
    };

    const poll = async () => {
      const answers = await Promise.all(
        keys.map((key) =>
          fetchOpenTickets(key).then(
            (found) => ({ found, error: null as string | null }),
            (e: Error) => ({ found: null, error: e.message }),
          ),
        ),
      );
      inFlight = false;
      if (disposed) return;

      // `Promise.all` answers in the order it was given, so the first key still wins a
      // repeated issue however the requests happened to race.
      const merged = mergeTickets(answers.map((a) => a.found ?? []));
      const errors = answers.map((a) => a.error).filter((e): e is string => e !== null);
      // Every key failed and nothing came back: keep whatever the column already has rather
      // than blanking it, and say why. A partial answer still replaces the list, because the
      // keys that did answer are authoritative for their own owners.
      if (merged.length > 0 || errors.length === 0) setTickets(merged);
      setError(errors.length > 0 ? errors.join("; ") : null);
      setLoading(false);
      arm(errors.length > 0 ? ERROR_MS : POLL_MS);
    };

    tick();

    // Wi-Fi came back, or the tab did. Either way the list on screen is as old as the sleep
    // was long, so ask now instead of waiting out the rest of the interval.
    const onOnline = () => tick();
    const onVisible = () => {
      if (document.visibilityState === "visible") tick();
    };
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      disposed = true;
      window.clearTimeout(timer);
      window.removeEventListener("online", onOnline);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [keysKey]);

  return { tickets, error, loading };
}

/** Every distinct Linear key on the presets, in config order.
 *
 *  Deduplicated because two presets sharing one key would otherwise return the same person's
 *  issues twice, and the merge below would have to undo it. */
export function linearKeys(presets: { linearKey: string }[]): string[] {
  const seen = new Set<string>();
  const keys: string[] = [];
  for (const preset of presets) {
    const key = (preset.linearKey ?? "").trim();
    if (key === "" || seen.has(key)) continue;
    seen.add(key);
    keys.push(key);
  }
  return keys;
}

/** One list out of several keys' answers, in key order.
 *
 *  The first key to report an issue owns its position. Two people assigned to the same issue
 *  is unusual but not impossible, and the column should draw it once either way. Identifiers
 *  are compared case-insensitively, as they are everywhere else. */
export function mergeTickets(lists: LinearTicket[][]): LinearTicket[] {
  const seen = new Set<string>();
  const merged: LinearTicket[] = [];
  for (const list of lists) {
    for (const ticket of list) {
      const id = ticket.id.toLowerCase();
      if (seen.has(id)) continue;
      seen.add(id);
      merged.push(ticket);
    }
  }
  return merged;
}
