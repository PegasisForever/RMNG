// The ticket module: every Linear ticket the control panel draws, what it can do to them, and
// the keys it does it with.
//
// It takes the presets (their Linear keys) and the clones; it gives the queue the Tickets tab
// draws, the ticket open in it, each clone's live ticket, the team's states and labels for a
// panel, where a parent or sub-issue link goes, and the writes: edit, move to another state,
// label, create. Each write shows at once, goes to Linear with the keys for the ticket's team,
// is put back if Linear refuses it, and asks Linear again once it lands. Linear is a port
// (`./port.ts`), so a test drives all of it with the in-memory one.
//
// The presets come from `GET /api/config` at mount, and again from Settings each time it
// saves (`setPresets`): a key typed there is asked with at once, without a reload.
//
// The list itself: each configured key's own open issues, plus the ticket behind each clone
// that has one, merged and kept current by a poll the browser runs itself.
//
// The clone-linked half is a second phase of the same round rather than a second poll, because
// the two lists overlap: a clone usually sits on a ticket that is In Progress and assigned to
// the operator, which the open query already returned. Only what that query did not carry is
// asked for by identifier, so the usual fleet costs no extra requests at all.
//
// That second phase runs on its own far slower clock. What it asks about are the tickets no
// open queue carries: closed, cancelled, backlogged, or somebody else's, none of which change
// on their own. Re-reading them every minute cost 11 of the 19 requests a minute this page
// spent on a 39-clone fleet, all of it to learn that a finished ticket was still finished.
//
// Each lookup asks the workspace's own key first. Every key is personal and sees one
// workspace, so asking in config order spends a request per key that could never have seen the
// ticket: on that same fleet a `WE` ticket cost three requests, two of them refusals.
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
// Two things keep the list honest between polls. A refetch follows every write, so an edit or
// a new ticket is on screen in one round trip rather than at the end of the interval. A
// watchdog interval runs regardless of what the fetch in flight claims about itself, the same
// discipline `useLiveState` uses on the SSE socket: a request that never settles is the one
// failure that would otherwise freeze the column for as long as the tab stayed open.
//
// The team's states and labels and the keys' workspaces are asked once per key list, with no
// poll: they change when somebody sits down and edits them. A lookup that fails answers empty
// lists and says nothing; a new key list asks again.

import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import { getConfig } from "~/lib/api";
import { openTicket } from "~/lib/linear/intake";
import { issueRefOf } from "~/lib/linear/issues";
import { type IssuePatch, type IssueRef, keysForTeam, type NewIssue, teamKeyOf } from "~/lib/linear/mutations";
import { linear, type LinearPort } from "~/lib/linear/port";
import type { TeamMeta } from "~/lib/linear/team";
import type { LinearTicket, LinearWorkspace, TicketLabel, TicketState, TicketWorkflowState } from "~/lib/linear/types";
import { mergeWorkspaces } from "~/lib/linear/workspaces";
import {
  cloneForTicket,
  type CloneTicket,
  cloneTickets,
  findTicket,
  openTickets,
  orderTickets,
  queued,
  ticketAfter,
} from "~/lib/tickets";
import type { Clone } from "~/lib/types";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

/** A preset as the poll reads one: the key to ask with, and the team keys that key claims.
 *  The labels are what put a ticket's own workspace first, so a lookup spends one request
 *  rather than one per configured account. */
export interface LinearPreset {
  labels?: string[];
  linearKey: string;
}

/** What one clone-linked identifier last resolved to, and when it was asked. */
interface LinkedHit {
  ticket: LinearTicket | null;
  at: number;
}

/** How often to ask Linear. Fast enough that a ticket someone closes is gone before you
 *  wonder why it is still there. */
const POLL_MS = 60_000;

/** How long a clone-linked ticket is trusted before it is read again. Ten minutes rather than
 *  one, because a ticket the open queues do not carry is finished, filed, or someone else's,
 *  and none of those change without a person doing it. A write goes on screen through `upsert`
 *  in the same frame, so this delay is never what the operator waits on. */
const LINKED_MS = 10 * 60_000;

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

/** Someone is looking at this tab. Always, where there is no page (a test). */
const visible = () => typeof document === "undefined" || document.visibilityState === "visible";

/** What a ticket's team offers it: the panel's two menus. */
export interface TeamMetaState {
  /** Every label the ticket's team can carry, sorted by name. */
  labels: TicketLabel[];
  /** The team's workflow, in its own order. */
  states: TicketWorkflowState[];
  /** The lookup is in flight. A menu says so rather than claiming the team offers nothing. */
  loading: boolean;
}

const NO_META: TeamMetaState = { labels: [], states: [], loading: false };

/** What the store holds. A new object on every change. */
export interface TicketSnapshot {
  /** The presets the keys come off (`config.presets`). */
  presets: PresetRedacted[];
  /** Every open issue across the configured keys plus every clone-linked one asked for by
   *  identifier, deduplicated. Empty until the first answer lands, and unchanged by a poll in
   *  which every key failed. */
  tickets: LinearTicket[];
  /** Why the last poll failed, if it did. Set alongside a partial list when only some keys
   *  answered. */
  error: string | null;
  /** True until the first answer, success or failure, has landed. The column reads it to tell
   *  "still asking" apart from "nothing to show". */
  loading: boolean;
  /** Every distinct workspace the keys belong to, in config order. */
  workspaces: LinearWorkspace[];
}

/** The tickets and the poll that keeps them, with no React in it. */
export interface TicketStore {
  /** The Linear the store asks. */
  port: LinearPort;
  get: () => TicketSnapshot;
  subscribe: (listener: () => void) => () => void;
  /** New presets: a change of keys starts a new poll and asks for the workspaces again. */
  setPresets: (presets: PresetRedacted[]) => void;
  /** The identifier of every clone that has one. Each is resolved in whatever state Linear
   *  holds it, which is what puts a clone whose ticket is Done or Cancelled in the list: the
   *  open query answers for neither, and the clone still draws its title. A new one is asked
   *  for a round trip later rather than at the end of the interval. */
  setLinked: (ids: string[]) => void;
  /** Ask Linear now, superseding whatever round is in flight. */
  refetch: () => void;
  /** Write one ticket into the list without asking Linear, matched by identifier: a write's
   *  change on screen in the same frame as the click. Linear's own answer replaces it a round
   *  trip later. */
  upsert: (ticket: LinearTicket) => void;
  /** Show `next` now, run `call` with the keys for the ticket's team, put `ticket` back if it
   *  is refused (and reject), ask Linear again once it lands. */
  write: (ticket: LinearTicket, next: LinearTicket, call: (keys: string[]) => Promise<unknown>) => Promise<void>;
  /** What `ticket`'s team offers it; the first ask starts the lookup. Null asks nothing. */
  meta: (ticket: IssueRef | null) => TeamMetaState;
  /** Begin polling (the page is mounted). */
  start: () => void;
  /** Stop every timer, and drop every answer still on its way. */
  stop: () => void;
}

export function ticketStore(port: LinearPort = linear): TicketStore {
  let snap: TicketSnapshot = { presets: [], tickets: [], error: null, loading: true, workspaces: [] };
  const listeners = new Set<() => void>();
  const set = (patch: Partial<TicketSnapshot>) => {
    snap = { ...snap, ...patch };
    for (const l of listeners) l();
  };

  // What each clone-linked identifier last resolved to, and when: a spend record rather than
  // something drawn. The tickets themselves are in `snap.tickets`.
  const linkedSeen = new Map<string, LinkedHit>();
  let linked: string[] = [];
  let running = false;
  // What the running poll and workspace lookup were started for, as JSON; null: none runs
  let pollFor: string | null = null;
  let workspacesFor: string | null = null;
  let stopPoll = () => {};
  let stopWorkspaces = () => {};
  // The running poll's own "ask now"; nothing while none runs
  let forceTick = () => {};

  // The team lookups, by team and key list. A failed one is kept (as empty menus) so a render
  // does not ask again; a new key list is a new entry, and so a fresh attempt.
  const metas = new Map<string, TeamMetaState>();

  /** The keys the presets give, each with the team keys it claims. */
  const withLabels = () => snap.presets.map((p) => ({ labels: p.labels ?? [], linearKey: p.linearKey ?? "" }));

  /** Start what the presets and the linked ids now call for, and stop what they no longer do. */
  const follow = () => {
    if (!running) return;
    const asking = askablePresets(snap.presets);
    const wanted = linkedIds(linked);
    const forPoll = JSON.stringify([asking, wanted]);
    if (forPoll !== pollFor) {
      stopPoll();
      pollFor = forPoll;
      stopPoll = poll(asking, wanted);
    }
    const keys = linearKeys(snap.presets);
    const forWorkspaces = JSON.stringify(keys);
    if (forWorkspaces !== workspacesFor) {
      stopWorkspaces();
      workspacesFor = forWorkspaces;
      stopWorkspaces = askWorkspaces(keys);
    }
  };

  /** The poll for these keys and ids, until the function it returns is called. */
  const poll = (asking: LinearPreset[], wanted: string[]): (() => void) => {
    const keys = linearKeys(asking);
    // Set by the stop, so a fetch still in flight cannot write into the list or arm another
    // timer behind it
    let disposed = false;
    // Which round owns the list. A round that is superseded, by the watchdog or by a refetch,
    // loses the right to write even if its own fetches answer later.
    let round = 0;
    // When the current round started, or null when none is in flight.
    let startedAt: number | null = null;
    // When the next poll is expected. The watchdog uses it to notice a timer that never fired,
    // which is what a throttled background tab does to `setTimeout`.
    let dueAt = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;

    if (keys.length === 0) {
      set({ tickets: [], error: null, loading: false });
      forceTick = () => {};
      return () => {};
    }

    const arm = (ms: number) => {
      clearTimeout(timer);
      dueAt = Date.now() + ms;
      timer = setTimeout(() => tick(), ms);
    };

    /** Start a round, unless one is genuinely still running. `force` skips both guards: it
     *  means a write just landed, or the watchdog has decided the round in flight is gone. */
    const tick = (force = false) => {
      if (disposed) return;
      if (!force) {
        if (startedAt !== null && Date.now() - startedAt < ROUND_BUDGET_MS) return;
        if (!visible()) {
          arm(POLL_MS);
          return;
        }
      }
      void ask();
    };

    const ask = async () => {
      const mine = ++round;
      startedAt = Date.now();
      // Each key's open issues
      const answers = await Promise.all(
        keys.map((key) =>
          port.openTickets(key).then(
            (found) => ({ found, error: null as string | null }),
            (e: Error) => ({ found: null, error: e.message }),
          ),
        ),
      );
      // A superseded round answers into the void: whatever replaced it is newer, and its own
      // answer is the one on screen.
      if (disposed || mine !== round) return;

      // `Promise.all` answers in the order it was given, so the first key still wins a
      // repeated issue however the requests happened to race.
      const open = mergeTickets(answers.map((a) => a.found ?? []));
      const errors = answers.map((a) => a.error).filter((e): e is string => e !== null);

      // The clone-linked tickets the open lists did not carry, and of those, only the ones no
      // recent answer covers. A miss is silent: the clone falls back to the title and link it
      // stored when it was made, which is exactly what those fields are for, and a banner
      // about it would say nothing the operator can act on.
      const carried = new Set(open.map((t) => t.id.toLowerCase()));
      const missing = wanted.filter((id) => !carried.has(id));
      const due = dueForLookup(missing, linkedSeen, Date.now(), LINKED_MS);
      const fresh = await Promise.all(due.map((id) => fetchLinked(port, asking, id)));
      if (disposed || mine !== round) return;
      startedAt = null;

      const at = Date.now();
      due.forEach((id, i) => linkedSeen.set(id, { ticket: fresh[i], at }));
      // A clone that is gone stops being asked about, and stops being remembered with it.
      for (const id of [...linkedSeen.keys()]) {
        if (!wanted.includes(id)) linkedSeen.delete(id);
      }
      const extra = missing.map((id) => linkedSeen.get(id)?.ticket ?? null);

      // Every key failed and nothing came back: keep whatever the column already has rather
      // than blanking it, and say why. A partial answer still replaces the list, because the
      // keys that did answer are authoritative for their own owners.
      const found = extra.filter((t): t is LinearTicket => t !== null);
      // The linked tickets already on screen come last, so one whose own lookup failed this
      // round keeps the copy from the round before instead of blinking back to the stored
      // title. A clone that is gone takes its ticket with it: `wanted` no longer names it.
      set({
        ...(open.length > 0 || errors.length === 0
          ? { tickets: mergeTickets([open, found, linkedOnly(snap.tickets, wanted)]) }
          : {}),
        error: errors.length > 0 ? errors.join("; ") : null,
        loading: false,
      });
      arm(errors.length > 0 ? ERROR_MS : POLL_MS);
    };

    tick();
    forceTick = () => tick(true);

    // The watchdog. It runs on its own interval and trusts nothing the transport reports
    // about itself, which is the whole point: a `fetch` that never settles leaves the round
    // in flight forever, and every path that re-armed the timer runs after that fetch.
    const watchdog = setInterval(() => {
      if (disposed || !visible()) return;
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
      if (visible()) tick(true);
    };
    const page = typeof window === "undefined" ? null : window;
    page?.addEventListener("online", catchUp);
    page?.document.addEventListener("visibilitychange", catchUp);

    return () => {
      disposed = true;
      forceTick = () => {};
      clearTimeout(timer);
      clearInterval(watchdog);
      page?.removeEventListener("online", catchUp);
      page?.document.removeEventListener("visibilitychange", catchUp);
    };
  };

  /** The keys' workspaces, asked once. A key that cannot answer contributes nothing and says
   *  nothing: the menu this feeds is a convenience link. */
  const askWorkspaces = (keys: string[]): (() => void) => {
    if (keys.length === 0) {
      set({ workspaces: [] });
      return () => {};
    }
    let disposed = false;
    void Promise.all(keys.map((key) => port.workspace(key).catch(() => null))).then((found) => {
      if (!disposed) set({ workspaces: mergeWorkspaces(found) });
    });
    return () => {
      disposed = true;
    };
  };

  const upsert = (ticket: LinearTicket) => {
    // And into the linked record, or the next poll would draw the copy from before the write
    // and hold it for the rest of the window. This copy came from a write that succeeded, so
    // it is the truest one anybody has until the window is up.
    linkedSeen.set(ticket.id.toLowerCase(), { ticket, at: Date.now() });
    // The ticket came from Linear, so the column has an answer to draw whether or not the
    // first poll has landed yet.
    set({ tickets: upsertTicket(snap.tickets, ticket), loading: false });
  };

  return {
    port,
    get: () => snap,
    subscribe: (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    setPresets: (presets) => {
      set({ presets });
      follow();
    },
    setLinked: (ids) => {
      linked = ids;
      follow();
    },
    refetch: () => forceTick(),
    upsert,
    write: (ticket, next, call) => {
      upsert(next);
      return call(keysForTeam(withLabels(), ticket.team ?? "")).then(
        () => forceTick(),
        (e: Error) => {
          upsert(ticket);
          throw e;
        },
      );
    },
    meta: (ticket) => {
      // The team key comes off the ticket the same way every write derives it, so what the
      // menus offer and the key that would write it are chosen by one rule
      const team = ticket ? teamKeyOf(ticket) : "";
      const keys = team === "" ? [] : keysForTeam(withLabels(), team);
      if (keys.length === 0) return NO_META;
      const at = JSON.stringify([team, keys]);
      const hit = metas.get(at);
      if (hit) return hit;
      const asking = { ...NO_META, loading: true };
      metas.set(at, asking);
      const settle = (found: TeamMeta) => {
        metas.set(at, { ...found, loading: false });
        set({});
      };
      void port.teamMeta(keys, team).then(settle, () => settle(NO_META));
      return asking;
    },
    start: () => {
      running = true;
      follow();
    },
    stop: () => {
      running = false;
      stopPoll();
      stopWorkspaces();
      stopPoll = stopWorkspaces = () => {};
      pollFor = workspacesFor = null;
    },
  };
}

/** What a ticket panel is handed for one ticket (`~/components/TicketPanel`): the ticket, its
 *  team's menus, the writes, and where its links go. */
export interface TicketPanelBinding {
  ticket: LinearTicket;
  onTitleChange: (title: string) => void;
  onStateChange: (state: TicketWorkflowState) => void;
  stateOptions: TicketWorkflowState[];
  statesLoading: boolean;
  labelOptions: TicketLabel[];
  labelsLoading: boolean;
  onAddLabel: (label: TicketLabel) => void;
  onRemoveLabel: (label: TicketLabel) => void;
  resolveLink: (ticketId: string) => { title: string; open: () => void } | null;
}

/** What the ticket module needs from the control panel. */
export interface TicketInputs {
  /** Every clone, archived ones too. */
  clones: Clone[];
  /** The operator's own order of the queue (the board's ticket order). */
  order: string[];
  /** The ticket open in the panel (`?ticket=`), or null. */
  openId: string | null;
  /** Open a ticket in the panel, or close it (null). `replace`: in place of the current
   *  history entry, for a move the operator did not ask for by name. */
  onOpen: (ticketId: string | null, replace?: boolean) => void;
  /** Show the clone made for a ticket. */
  onShowClone: (clone: Clone) => void;
  /** A write settled: null when it went through, the reason when it was refused. */
  onError: (message: string | null) => void;
}

/** The ticket module, as the control panel uses it. */
export interface Tickets {
  /** The presets the keys come off: the new-ticket dialog's teams. */
  presets: PresetRedacted[];
  /** Settings saved: ask with these presets' keys from now on. */
  setPresets: (presets: PresetRedacted[]) => void;
  /** The queue the Tickets tab draws: open tickets with no clone, in the operator's order. */
  visible: LinearTicket[];
  /** The ticket open in the panel, if it is in the queue. */
  open: LinearTicket | null;
  /** Each clone's ticket as Linear has it now, by clone id. */
  forClones: Record<string, CloneTicket>;
  error: string | null;
  loading: boolean;
  workspaces: LinearWorkspace[];
  /** Any ticket the module has (a clone's too), by identifier. */
  find: (ticketId: string) => LinearTicket | null;
  edit: (ticket: LinearTicket, patch: IssuePatch) => void;
  /** Into the first state of a kind (Backlog, Canceled). */
  moveTo: (ticket: LinearTicket, state: TicketState) => void;
  /** Into one state of the team's workflow. */
  setState: (ticket: LinearTicket, state: TicketWorkflowState) => void;
  setLabel: (ticket: LinearTicket, label: TicketLabel, on: boolean) => void;
  /** Open a new issue; once Linear has it, it is in the list and open in the panel. Rejects
   *  with Linear's refusal. */
  create: (issue: NewIssue) => Promise<LinearTicket>;
  /** What `ticket`'s team offers it. */
  meta: (ticket: LinearTicket | null) => TeamMetaState;
  /** Where a parent or sub-issue goes: the ticket in the queue (`inQueue`), or the clone made
   *  for it; null: Linear. */
  resolveLink: (ticketId: string, inQueue: boolean) => { title: string; open: () => void } | null;
  /** A ticket panel's data and writes for `ticket`; `own`: a clone's own ticket, whose links
   *  never open the queue. */
  panel: (ticket: LinearTicket, own: boolean) => TicketPanelBinding;
}

/** The module over `store` as it stands (`snap`), for these inputs. */
export function ticketsOf(store: TicketStore, snap: TicketSnapshot, inputs: TicketInputs): Tickets {
  const { clones, openId, onOpen, onError } = inputs;
  const visibleList = orderTickets(openTickets(snap.tickets, clones), inputs.order);
  const open = openId ? findTicket(openId, visibleList) : null;
  const run = (p: Promise<unknown>) => void p.then(() => onError(null), (e: Error) => onError(e.message));
  const write = (t: LinearTicket, next: LinearTicket, call: (keys: string[]) => Promise<unknown>) =>
    run(store.write(t, next, call));
  /** The open ticket leaves the queue: the panel goes to the one after it. */
  const advancePast = (t: LinearTicket) => {
    if (open?.id !== t.id) return;
    onOpen(ticketAfter(visibleList, t.id)?.id ?? null, true);
  };

  const edit = (t: LinearTicket, patch: IssuePatch) =>
    write(t, { ...t, ...patch }, (keys) => store.port.update(keys, t, patch));
  const setState = (t: LinearTicket, s: TicketWorkflowState) => {
    if (!queued(s.type)) advancePast(t);
    write(t, { ...t, state: s.type, stateId: s.id, stateName: s.name }, (keys) => store.port.setStateId(keys, t, s.id));
  };
  const setLabel = (t: LinearTicket, label: TicketLabel, on: boolean) =>
    write(t, { ...t, labels: on ? [...t.labels, label] : t.labels.filter((l) => l.id !== label.id) }, (keys) =>
      store.port.setLabel(keys, t, label.id, on),
    );
  const resolveLink = (id: string, inQueue: boolean) => {
    const t = inQueue ? findTicket(id, visibleList) : null;
    if (t) return { title: `Show ${t.id}`, open: () => onOpen(t.id) };
    const clone = cloneForTicket(id, clones);
    return clone ? { title: `Show the clone for ${id}: ${clone.id}`, open: () => inputs.onShowClone(clone) } : null;
  };

  return {
    presets: snap.presets,
    setPresets: store.setPresets,
    visible: visibleList,
    open,
    forClones: cloneTickets(clones, snap.tickets),
    error: snap.error,
    loading: snap.loading,
    workspaces: snap.workspaces,
    find: (id) => findTicket(id, snap.tickets),
    edit,
    moveTo: (t, next) => {
      if (!queued(next)) advancePast(t);
      write(t, { ...t, state: next }, (keys) => store.port.setState(keys, t, next));
    },
    setState,
    setLabel,
    create: (issue) =>
      openTicket(snap.presets, issue, store.port).then((created) => {
        store.upsert(created);
        onOpen(created.id);
        store.refetch();
        return created;
      }),
    meta: (t) => store.meta(t),
    resolveLink,
    panel: (t, own) => {
      const meta = store.meta(t);
      return {
        ticket: t,
        onTitleChange: (title) => edit(t, { title }),
        onStateChange: (s) => setState(t, s),
        stateOptions: meta.states,
        statesLoading: meta.loading,
        labelOptions: meta.labels,
        labelsLoading: meta.loading,
        onAddLabel: (l) => setLabel(t, l, true),
        onRemoveLabel: (l) => setLabel(t, l, false),
        resolveLink: (id) => resolveLink(id, !own),
      };
    },
  };
}

/** The ticket module, live: the presets from `GET /api/config` at mount, the poll while the
 *  control panel is mounted. `port`: Linear, unless a test passes another. */
export function useTickets(inputs: TicketInputs, port: LinearPort = linear): Tickets {
  const [store] = useState(() => ticketStore(port));
  const snap = useSyncExternalStore(store.subscribe, store.get, store.get);
  const onError = useRef(inputs.onError);
  onError.current = inputs.onError;

  useEffect(() => {
    store.start();
    getConfig()
      .then((c) => store.setPresets(c.presets))
      .catch((e: Error) => onError.current(e.message));
    return store.stop;
  }, [store]);

  // Keyed on the serialized list: the clones are a fresh array on every frame
  const linkedKey = JSON.stringify(linkedIds(inputs.clones.flatMap((c) => (c.linearTicket ? [c.linearTicket] : []))));
  useEffect(() => store.setLinked(JSON.parse(linkedKey) as string[]), [store, linkedKey]);

  return ticketsOf(store, snap, inputs);
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

/** Every distinct clone-linked identifier, lowercased, in the order given.
 *
 *  Lowercased because that is how every identifier is compared in this app, and deduplicated
 *  because two clones on one ticket is a normal thing to do and would otherwise be two
 *  requests for one answer. */
export function linkedIds(ids: string[]): string[] {
  const seen = new Set<string>();
  const kept: string[] = [];
  for (const raw of ids) {
    const id = raw.trim().toLowerCase();
    if (id === "" || seen.has(id)) continue;
    seen.add(id);
    kept.push(id);
  }
  return kept;
}

/** The tickets in `current` that `wanted` still names, in the order they were in.
 *
 *  What carries a clone's ticket across a round that could not re-read it. Nothing else in the
 *  list survives on its own: an open ticket that stops being open is meant to disappear. */
export function linkedOnly(current: LinearTicket[], wanted: string[]): LinearTicket[] {
  const keep = new Set(wanted);
  return current.filter((t) => keep.has(t.id.toLowerCase()));
}

/** One clone-linked ticket by identifier, or null when no key can see it.
 *
 *  The keys are ordered by the ticket's own team, so the account that owns the workspace is
 *  asked first and the usual lookup costs exactly one request. Asking in config order instead
 *  spent one on every account listed ahead of it, each answering "not found" and each counted
 *  against that account's own hourly budget.
 *
 *  Null rather than a throw, because the caller is a poll that has to finish either way. An
 *  identifier that parses into nothing takes no request at all. */
async function fetchLinked(port: LinearPort, presets: LinearPreset[], id: string): Promise<LinearTicket | null> {
  const ref = issueRefOf(id);
  if (!ref) return null;
  const keys = keysForTeam(
    presets.map((p) => ({ labels: p.labels ?? [], linearKey: p.linearKey })),
    ref.prefix,
  );
  return port.ticket(keys, ref).catch(() => null);
}

/** The identifiers worth a request this round: the ones nothing has answered for yet, and the
 *  ones whose answer has aged past `ttl`.
 *
 *  Pure, and the whole of the second cadence. A clone that has just been made has no entry, so
 *  its ticket is read on the very next round rather than up to `ttl` later. */
export function dueForLookup(
  wanted: string[],
  seen: Map<string, { at: number }>,
  now: number,
  ttl: number,
): string[] {
  return wanted.filter((id) => {
    const hit = seen.get(id);
    return hit === undefined || now - hit.at >= ttl;
  });
}

/** Every preset worth asking with, in config order, each carrying the team keys it claims.
 *
 *  Presets with no key are dropped here rather than at each use. Duplicates are kept, because
 *  two presets can share one key and claim different teams, and `keysForTeam` dedupes by key
 *  while it orders. */
export function askablePresets(presets: LinearPreset[]): LinearPreset[] {
  return presets
    .map((p) => ({ labels: p.labels ?? [], linearKey: (p.linearKey ?? "").trim() }))
    .filter((p) => p.linearKey !== "");
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
