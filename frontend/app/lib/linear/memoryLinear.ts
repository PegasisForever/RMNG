// Linear in memory, behind the same port as the real one (`./port.ts`), for tests. Each key
// sees its own issues and each team has its own workflow and labels; a write changes the issue
// it names, and `refuse` makes every write fail the way Linear refuses one.
import { resolvedFromTicket } from "~/lib/linear/issues";
import { type IssueRef, teamKeyOf } from "~/lib/linear/mutations";
import type { LinearPort } from "~/lib/linear/port";
import { queued } from "~/lib/tickets";
import type { TeamMeta } from "~/lib/linear/team";
import type { LinearTicket, LinearWorkspace } from "~/lib/linear/types";

export interface MemoryLinear extends LinearPort {
  /** Each key's issues, as they stand now. */
  issues: Record<string, LinearTicket[]>;
  /** Every call, as `"<call> <key or identifier>"`, in order. */
  calls: string[];
  /** Every write fails while this is set. */
  refuse: boolean;
}

export function memoryLinear({
  issues = {},
  teams = {},
  workspaces = {},
}: {
  issues?: Record<string, LinearTicket[]>;
  /** Each team's workflow and labels, by uppercase team key. */
  teams?: Record<string, TeamMeta>;
  workspaces?: Record<string, LinearWorkspace>;
} = {}): MemoryLinear {
  const sameId = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
  const holder = (keys: string[], id: string) => keys.find((k) => (m.issues[k] ?? []).some((t) => sameId(t.id, id)));

  /** Change the issue `ticket` names, as the first of `keys` that holds it. */
  const write = async (call: string, keys: string[], ticket: IssueRef, change: (t: LinearTicket) => LinearTicket) => {
    m.calls.push(`${call} ${ticket.id}`);
    if (m.refuse) throw new Error(`Linear refused the ${call} on ${ticket.id}`);
    const key = holder(keys, ticket.id);
    if (!key) throw new Error(`Linear has no issue ${ticket.id} for this key`);
    m.issues[key] = m.issues[key].map((t) => (sameId(t.id, ticket.id) ? change(t) : t));
  };

  const m: MemoryLinear = {
    issues,
    calls: [],
    refuse: false,
    create: async (key, issue) => {
      m.calls.push(`create ${issue.team}`);
      if (m.refuse) throw new Error(`Linear refused a new ticket in ${issue.team}`);
      const list = m.issues[key] ?? [];
      const id = `${issue.team.toUpperCase()}-${100 + list.length}`;
      const ticket: LinearTicket = {
        id,
        title: issue.title,
        url: `https://linear.app/x/issue/${id}`,
        state: "todo",
        team: issue.team.toUpperCase(),
        description: issue.description,
        labels: [],
        children: [],
      };
      m.issues[key] = [ticket, ...list];
      return ticket;
    },
    find: async (keys, ref) => {
      m.calls.push(`find ${ref.identifier}`);
      const key = holder(keys, ref.identifier);
      if (!key) throw new Error(`no issue ${ref.identifier}`);
      return { issue: resolvedFromTicket(m.issues[key].find((t) => sameId(t.id, ref.identifier))!), key };
    },
    start: async () => false,
    people: async () => [],
    openTickets: async (key) => {
      m.calls.push(`open ${key}`);
      if (!(key in m.issues)) throw new Error("Linear refused the key");
      return m.issues[key].filter((t) => queued(t.state));
    },
    ticket: async (keys, ref) => {
      m.calls.push(`ticket ${ref.identifier}`);
      const key = holder(keys, ref.identifier);
      if (!key) throw new Error(`no issue ${ref.identifier}`);
      return m.issues[key].find((t) => sameId(t.id, ref.identifier))!;
    },
    update: (keys, ticket, patch) => write("update", keys, ticket, (t) => ({ ...t, ...patch })),
    setState: (keys, ticket, state) => write("state", keys, ticket, (t) => ({ ...t, state })),
    setStateId: (keys, ticket, stateId) =>
      write("state", keys, ticket, (t) => {
        const s = teams[teamKeyOf(ticket)]?.states.find((x) => x.id === stateId);
        if (!s) throw new Error(`no state ${stateId}`);
        return { ...t, state: s.type, stateId: s.id, stateName: s.name };
      }),
    setLabel: (keys, ticket, labelId, on) =>
      write("label", keys, ticket, (t) => {
        const label = teams[teamKeyOf(ticket)]?.labels.find((l) => l.id === labelId);
        if (!label) throw new Error(`no label ${labelId}`);
        return { ...t, labels: on ? [...t.labels, label] : t.labels.filter((l) => l.id !== labelId) };
      }),
    teamMeta: async (keys, team) => {
      m.calls.push(`team ${team}`);
      const meta = teams[team.toUpperCase()];
      if (!meta || keys.length === 0) throw new Error(`this Linear key cannot see team ${team}`);
      return meta;
    },
    workspace: async (key) => {
      m.calls.push(`workspace ${key}`);
      return workspaces[key] ?? null;
    },
  };
  return m;
}
