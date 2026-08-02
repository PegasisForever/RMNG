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
//
// Two things keep the list honest between polls. `refetch` is called after every write, so an
// edit or a new ticket is on screen in one round trip rather than at the end of the interval.
// A watchdog interval runs regardless of what the fetch in flight claims about itself, the
// same discipline `useLiveState` uses on the SSE socket: a request that never settles is the
// one failure that would otherwise freeze the column for as long as the tab stayed open.

import { useCallback, useEffect, useRef, useState } from "react";

import { fetchOpenTickets } from "~/lib/linear/queries";
import type { LinearTicket } from "~/lib/linear/types";

/** How often to ask Linear. Fast enough that a ticket someone closes is gone before you
 *  wonder why it is still there. */
const POLL_MS = 60_000;

/** How long to wait after a failure. Linear being down is not urgent, the column keeps
 *  drawing the last good list, so this backs off rather than hammering. */
const ERROR_MS = 5 * 60_000;

/** How long one round of fetches may run before the watchdog gives up on it and starts
 *  another. Comfortably past `gql`'s own 20-second deadline, so a request that answers late
 *  is not raced against its own replacement, and well inside the poll interval so a round
 *  that never settles costs one interval rather than every one after it. */
const ROUND_BUDGET_MS = 30_000;

/** How often the watchdog looks. Same cadence the SSE watchdog uses. */
const WATCHDOG_MS = 5_000;

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
  /** Ask Linear now, superseding whatever round is in flight. Every write calls it: without
   *  it a ticket just created or just edited is invisible until the interval elapses. */
  refetch: () => void;
  /** Write one ticket into the list without asking Linear, matched by identifier.
   *
   *  The optimistic half of a write. `refetch` is what makes the change real. This is what
   *  puts it on screen in the same frame as the click, and Linear's own answer replaces it a
   *  round trip later. */
  upsert: (ticket: LinearTicket) => void;
}

/** The ticket column's list, live.
 *
 *  Pass the presets from `GET /api/config`; the keys are read off them and nothing else is.
 *  A preset with no key is skipped, and two presets sharing one key are asked once. */
export function useTickets(presets: { linearKey: string }[]): Tickets {
  const [tickets, setTickets] = useState<LinearTicket[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // The effect owns the fetching, and `refetch` is called from render callbacks that outlive
  // any one of its runs. The ref is the handoff: the effect publishes its own `tick` here and
  // takes it back down on unmount, so a callback fired after that asks nothing.
  const forceTick = useRef<() => void>(() => {});

  // The effect restarts when the set of keys changes, which is what picks up a key the
  // operator just typed into Settings. Keyed on the serialized list rather than the array,
  // because `presets` is a fresh array on every render that reaches this hook.
  const keysKey = JSON.stringify(linearKeys(presets));

  useEffect(() => {
    const keys = JSON.parse(keysKey) as string[];
    // Set on unmount, so a fetch still in flight cannot write state into a gone component or
    // arm another timer behind it. Same discipline as the SSE socket's own effect.
    let disposed = false;
    // Which round owns the state. A round that is superseded, by the watchdog or by a
    // refetch, loses the right to write even if its own fetches answer later.
    let round = 0;
    // When the current round started, or null when none is in flight.
    let startedAt: number | null = null;
    // When the next poll is expected. The watchdog uses it to notice a timer that never fired,
    // which is what a throttled background tab does to `setTimeout`.
    let dueAt = 0;
    let timer: number | undefined;

    if (keys.length === 0) {
      setTickets([]);
      setError(null);
      setLoading(false);
      return;
    }

    const arm = (ms: number) => {
      window.clearTimeout(timer);
      dueAt = Date.now() + ms;
      timer = window.setTimeout(() => tick(), ms);
    };

    /** Start a round, unless one is genuinely still running. `force` skips both guards: it
     *  means a write just landed, or the watchdog has decided the round in flight is gone. */
    const tick = (force = false) => {
      if (disposed) return;
      if (!force) {
        if (startedAt !== null && Date.now() - startedAt < ROUND_BUDGET_MS) return;
        if (document.visibilityState !== "visible") {
          arm(POLL_MS);
          return;
        }
      }
      void poll();
    };

    const poll = async () => {
      const mine = ++round;
      startedAt = Date.now();
      const answers = await Promise.all(
        keys.map((key) =>
          fetchOpenTickets(key).then(
            (found) => ({ found, error: null as string | null }),
            (e: Error) => ({ found: null, error: e.message }),
          ),
        ),
      );
      // A superseded round answers into the void: whatever replaced it is newer, and its own
      // answer is the one on screen.
      if (disposed || mine !== round) return;
      startedAt = null;

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
    forceTick.current = () => tick(true);

    // The watchdog. It runs on its own interval and trusts nothing the transport reports
    // about itself, which is the whole point: a `fetch` that never settles leaves the round
    // in flight forever, and every path that re-armed the timer runs after that fetch.
    const watchdog = window.setInterval(() => {
      if (disposed || document.visibilityState !== "visible") return;
      if (startedAt !== null) {
        // Past its budget, so treat it as gone and start a fresh round over it.
        if (Date.now() - startedAt > ROUND_BUDGET_MS) tick(true);
        return;
      }
      // Nothing in flight and the timer is overdue: it was throttled, or it never fired.
      if (dueAt !== 0 && Date.now() > dueAt + WATCHDOG_MS) tick();
    }, WATCHDOG_MS);

    // Wi-Fi came back, or the tab did. Either way the list on screen is as old as the sleep
    // was long, so ask now instead of waiting out the rest of the interval.
    // Forced, because the round in flight when the network dropped is the one to give up on.
    // Still only for a tab someone is looking at: a hidden tab asks nothing either way.
    const catchUp = () => {
      if (document.visibilityState === "visible") tick(true);
    };
    const onOnline = () => catchUp();
    const onVisible = () => catchUp();
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      disposed = true;
      forceTick.current = () => {};
      window.clearTimeout(timer);
      window.clearInterval(watchdog);
      window.removeEventListener("online", onOnline);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [keysKey]);

  const refetch = useCallback(() => forceTick.current(), []);
  const upsert = useCallback((ticket: LinearTicket) => {
    setTickets((current) => upsertTicket(current, ticket));
    // The ticket came from Linear, so the column has an answer to draw whether or not the
    // first poll has landed yet.
    setLoading(false);
  }, []);

  return { tickets, error, loading, refetch, upsert };
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

/** The list with one ticket written into it, replacing the entry that shares its identifier
 *  or joining the front when none does.
 *
 *  The front, because the query behind the list is newest first and a ticket written here was
 *  just created or just edited. Position within the column is the operator's own order
 *  anyway, which `orderTickets` applies over this. */
export function upsertTicket(current: LinearTicket[], ticket: LinearTicket): LinearTicket[] {
  const id = ticket.id.toLowerCase();
  const at = current.findIndex((t) => t.id.toLowerCase() === id);
  if (at === -1) return [ticket, ...current];
  const next = [...current];
  next[at] = ticket;
  return next;
}
