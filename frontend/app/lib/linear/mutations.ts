// The four writes the board can start: open an issue, edit one, move one to another workflow
// state, and put a label on one or take it off.
//
// All four used to be the server's, or nobody's. The browser holds the preset keys now, so it
// addresses Linear itself and the control-server never sees a ticket body. What crossed over
// unchanged is the part that is not obvious from the mutation names:
//
// - A new issue is pinned to the team's lowest-`position` `unstarted` state. The column draws
//   Todo and In Progress only, so an issue left in whatever state the team defaults to (very
//   often Backlog) would be created and disappear in the same breath.
// - A new issue is assigned to whoever the caller named, and to the key's own owner when it
//   named nobody. That fallback is why the column works: it lists what the owner is assigned,
//   so an unassigned issue would never appear in it. An issue assigned to somebody else will
//   not appear in it either, which the dialog says out loud before it is opened.
// - An edit addresses the issue by Linear's UUID, never by `WE-142`.
//
// A state is picked the same way whichever write is asking: a workflow can hold several states
// of one type, and Linear orders them by `position`, so the lowest is the one its own board
// would land the issue in.
//
// Everything that decides a variable is pure and exported. The network calls below are thin
// wrappers over those, so the rules are testable without a key.

import { gql } from "~/lib/linear/client";
import { stateTypeOf } from "~/lib/linear/issues";
import { OPEN_ISSUE_FIELDS, ticketFromNode } from "~/lib/linear/queries";
import type { LinearTicket, TicketState } from "~/lib/linear/types";

/** What the new-ticket dialog sends. `priority` is Linear's own scale: 1 urgent, 2 high,
 *  3 medium, 4 low. Zero and absent both mean no priority, which is a real value there. */
export interface NewIssue {
  /** Team key, e.g. `WE`. Also what picks the key that opens it. */
  team: string;
  title: string;
  description: string;
  priority?: number;
  /** Who to assign it to, as Linear's user UUID. Absent assigns it to the key's own owner,
   *  which is what every caller wanted before the dialog offered a choice. */
  assigneeId?: string;
}

/** A title and/or a description. An omitted field is left alone. An empty description is a
 *  real value: clearing a body is something an operator does on purpose. */
export interface IssuePatch {
  title?: string;
  description?: string;
}

/** The issue a mutation addresses. `uuid` is what Linear takes. `id` is the `WE-142` the
 *  operator sees, and is enough to look the UUID up when a ticket carries none. */
export interface IssueRef {
  id: string;
  uuid?: string;
  /** Team key, e.g. `WE`. Names the workflow a state is picked out of. Absent falls back to
   *  the identifier's own team part, which is the same answer for every real ticket. */
  team?: string;
}

// --- what key writes what ----------------------------------------------------

/** Every distinct Linear key that could own team `team`, best guess first.
 *
 *  A preset labelled `WE` claims team WE, so its key is the one that opens and edits WE
 *  issues. That is the server's `pick_preset_by_prefix` rule, first match in config order.
 *  The remaining keys follow, because a label list is the operator's hint rather than a
 *  guarantee, and a key that can see the issue is a key that may write it.
 *
 *  A blank `team` asks for no preference and returns every key in config order. */
export function keysForTeam(
  presets: { labels: string[]; linearKey: string }[],
  team: string,
): string[] {
  const wanted = team.trim().toLowerCase();
  const claims = (p: { labels: string[] }) =>
    wanted !== "" && p.labels.some((l) => l.trim().toLowerCase() === wanted);

  const seen = new Set<string>();
  const keys: string[] = [];
  const take = (only: boolean) => {
    for (const preset of presets) {
      if (claims(preset) !== only) continue;
      const key = (preset.linearKey ?? "").trim();
      if (key === "" || seen.has(key)) continue;
      seen.add(key);
      keys.push(key);
    }
  };
  take(true);
  take(false);
  return keys;
}

// --- issueCreate -------------------------------------------------------------

/** The team's own id, its workflow states, and the key owner's user id in one round trip.
 *  Three separate queries would be three chances for one of them to be the slow one. */
const TEAM_FOR_CREATE_QUERY =
  "query($team: String!) { " +
  "teams(filter: { key: { eq: $team } }, first: 1) { " +
  "nodes { id states { nodes { id type position } } } } " +
  "viewer { id } }";

const ISSUE_CREATE_MUTATION =
  "mutation($input: IssueCreateInput!) { issueCreate(input: $input) { " +
  `success issue { ${OPEN_ISSUE_FIELDS} } } }`;

/** The lowest-`position` state of `type` in a team's workflow.
 *
 *  A team can have several states of one type, and Linear orders them by `position`, so the
 *  lowest is the one its own board would use. Null when the team declares none, which leaves
 *  the choice to Linear rather than inventing one.
 *
 *  Takes the raw `states.nodes` array, because nothing type-checked the answer. */
export function pickStateId(nodes: unknown, type: string): string | null {
  if (!Array.isArray(nodes)) return null;
  const matching = nodes.filter(
    (n) => typeof n === "object" && n !== null && (n as { type?: unknown }).type === type,
  ) as { id?: unknown; position?: unknown }[];
  // Stable, so two states sharing a position keep the order Linear listed them in. A state
  // with no position sorts last rather than first: an unknown rank is not rank zero.
  const ranked = [...matching].sort((a, b) => rank(a.position) - rank(b.position));
  const id = ranked[0]?.id;
  return typeof id === "string" && id !== "" ? id : null;
}

/** The state a new issue is pinned to: the team's first Todo.
 *
 *  The column draws Todo and In Progress only, so an issue left in whatever state the team
 *  defaults to would be created and disappear in the same breath. */
export function pickCreateStateId(nodes: unknown): string | null {
  return pickStateId(nodes, "unstarted");
}

function rank(position: unknown): number {
  return typeof position === "number" && Number.isFinite(position) ? position : Number.MAX_VALUE;
}

/** The `IssueCreateInput` for a new issue.
 *
 *  The assignee is whoever the caller named, and the key's own owner when it named nobody. That
 *  fallback is the rule the column depends on: it lists what the key owner is assigned, so an
 *  issue opened with no assignee at all would be created and never seen.
 *
 *  `assigneeId` is left off rather than sent as null when neither is known, which is the case
 *  for an app or OAuth actor. Creation still works. The issue is then unassigned and the column
 *  will not list it. Same for `stateId`. */
export function issueCreateInput(fields: {
  teamId: string;
  title: string;
  description: string;
  viewerId?: string | null;
  assigneeId?: string | null;
  stateId?: string | null;
  priority?: number;
}): Record<string, unknown> {
  const input: Record<string, unknown> = {
    teamId: fields.teamId,
    title: fields.title,
    description: fields.description,
  };
  const assignee = (fields.assigneeId ?? "").trim() || fields.viewerId;
  if (assignee) input.assigneeId = assignee;
  if (fields.stateId) input.stateId = fields.stateId;
  const priority = fields.priority;
  if (typeof priority === "number" && Number.isInteger(priority) && priority > 0) {
    input.priority = priority;
  }
  return input;
}

/** Open an issue and hand back the ticket the column draws.
 *
 *  Two round trips, in this order and not one: the state to pin it to and the user to assign
 *  it to are both properties of the team, and neither is knowable before the team is. */
export async function issueCreate(key: string, issue: NewIssue): Promise<LinearTicket> {
  // The same guard `issueUpdate` states, said here rather than one layer down in `gql`, so a
  // caller reading this signature can see that a blank key is refused and not sent. The
  // dialog reaches this with `?? ""` when no preset holds a key for the chosen team.
  if (key.trim() === "") throw new Error("no Linear API key configured for that workspace");

  const team = issue.team.trim().toUpperCase();
  const data = await gql<{
    teams?: { nodes?: unknown[] };
    viewer?: { id?: string };
  }>(key, TEAM_FOR_CREATE_QUERY, { team });

  const node = data.teams?.nodes?.[0] as
    | { id?: unknown; states?: { nodes?: unknown } }
    | undefined;
  if (!node) throw new Error(`team ${team} not found`);
  const teamId = node.id;
  if (typeof teamId !== "string" || teamId === "") throw new Error(`team ${team} has no id`);

  const input = issueCreateInput({
    teamId,
    title: issue.title,
    description: issue.description,
    viewerId: data.viewer?.id ?? null,
    assigneeId: issue.assigneeId ?? null,
    stateId: pickCreateStateId(node.states?.nodes),
    priority: issue.priority,
  });

  const created = await gql<{ issueCreate?: { success?: boolean; issue?: unknown } }>(
    key,
    ISSUE_CREATE_MUTATION,
    { input },
  );
  const ticket = created.issueCreate?.success ? ticketFromNode(created.issueCreate.issue) : null;
  if (!ticket) throw new Error(`Linear refused a new ticket in ${team}`);
  return ticket;
}

// --- issueUpdate -------------------------------------------------------------

const ISSUE_UPDATE_MUTATION =
  "mutation($id: String!, $input: IssueUpdateInput!) { issueUpdate(id: $id, input: $input) { success } }";

/** Linear takes an identifier here as well as a UUID, which is what makes it the fallback
 *  for a ticket that reached the browser without one. */
const ISSUE_UUID_QUERY = "query($id: String!) { issue(id: $id) { id } }";

/** The `IssueUpdateInput` for a patch. Empty when the patch asks for nothing, which is the
 *  caller's signal that there is no mutation to send. */
export function issueUpdateInput(patch: IssuePatch): Record<string, unknown> {
  const input: Record<string, unknown> = {};
  if (patch.title !== undefined) input.title = patch.title;
  if (patch.description !== undefined) input.description = patch.description;
  return input;
}

/** Run `write` against each key in turn until one lands.
 *
 *  Mirrors the server's rule that a key which can see an issue is the key that may write it. A
 *  workspace's own key is put first by `keysForTeam`, so the usual fleet spends exactly one
 *  request here.
 *
 *  Re-trying against a second key cannot double-apply anything: every write below sets a value
 *  rather than applying a delta, so doing it twice is doing it once. */
async function withFirstKey(
  keys: string[],
  refused: string,
  write: (key: string) => Promise<void>,
): Promise<void> {
  if (keys.length === 0) throw new Error("no Linear API key configured for that workspace");
  let last: Error | null = null;
  for (const key of keys) {
    try {
      await write(key);
      return;
    } catch (e) {
      last = e as Error;
    }
  }
  throw last ?? new Error(refused);
}

/** Write a title and/or description onto an issue. */
export async function issueUpdate(
  keys: string[],
  ticket: IssueRef,
  patch: IssuePatch,
): Promise<void> {
  const input = issueUpdateInput(patch);
  if (Object.keys(input).length === 0) return;
  const refused = `Linear refused the update to ${ticket.id}`;
  await withFirstKey(keys, refused, async (key) => {
    const uuid = await issueUuid(key, ticket);
    const data = await gql<{ issueUpdate?: { success?: boolean } }>(key, ISSUE_UPDATE_MUTATION, {
      id: uuid,
      input,
    });
    if (data.issueUpdate?.success !== true) throw new Error(refused);
  });
}

// --- issueUpdate { stateId } --------------------------------------------------

/** A team's whole workflow, by team key. `position` is what orders two states of one type. */
const TEAM_STATES_QUERY =
  "query($team: String!) { teams(filter: { key: { eq: $team } }, first: 1) { " +
  "nodes { states { nodes { id type position } } } } }";

/** The team key a state lookup is filtered by: the ticket's own, or the part of its identifier
 *  before the number. Uppercase, which is how Linear stores it. */
export function teamKeyOf(ticket: IssueRef): string {
  const team = (ticket.team ?? "").trim();
  const dash = ticket.id.lastIndexOf("-");
  return (team !== "" ? team : dash > 0 ? ticket.id.slice(0, dash) : ticket.id).toUpperCase();
}

/** Move an issue into `state`.
 *
 *  Two round trips per key: the state ids belong to the team's workflow and are not knowable
 *  before it is asked. The state is named by type rather than by name, so a workspace calling
 *  its backlog "Icebox" is moved into that one.
 *
 *  Nothing here drops the ticket from the column. The caller does that on its own, because
 *  waiting out a poll would leave a card sitting there that was cancelled a second ago. */
export async function issueSetState(
  keys: string[],
  ticket: IssueRef,
  state: TicketState,
): Promise<void> {
  const type = stateTypeOf(state);
  const team = teamKeyOf(ticket);
  const refused = `Linear refused the state change on ${ticket.id}`;
  await withFirstKey(keys, refused, async (key) => {
    const found = await gql<{ teams?: { nodes?: unknown[] } }>(key, TEAM_STATES_QUERY, { team });
    const node = found.teams?.nodes?.[0] as { states?: { nodes?: unknown } } | undefined;
    const stateId = pickStateId(node?.states?.nodes, type);
    if (stateId === null) throw new Error(`team ${team} has no ${type} state`);

    const uuid = await issueUuid(key, ticket);
    const data = await gql<{ issueUpdate?: { success?: boolean } }>(key, ISSUE_UPDATE_MUTATION, {
      id: uuid,
      input: { stateId },
    });
    if (data.issueUpdate?.success !== true) throw new Error(refused);
  });
}

/** Move an issue into the state `stateId` names.
 *
 *  The panel's own move, and the one the operator picked by name off the team's workflow. It
 *  takes a state id rather than a type because that is the whole point of listing the real
 *  states: a team with both "In Progress" and "In Review" has two `started` states, and
 *  [`issueSetState`] can only ever reach the first of them.
 *
 *  One round trip instead of that function's two, the state having already been looked up when
 *  the menu was filled. */
export async function issueSetStateId(
  keys: string[],
  ticket: IssueRef,
  stateId: string,
): Promise<void> {
  if (stateId.trim() === "") {
    throw new Error(`that state carries no Linear id, so ${ticket.id} cannot be moved`);
  }
  const refused = `Linear refused the state change on ${ticket.id}`;
  await withFirstKey(keys, refused, async (key) => {
    const uuid = await issueUuid(key, ticket);
    const data = await gql<{ issueUpdate?: { success?: boolean } }>(key, ISSUE_UPDATE_MUTATION, {
      id: uuid,
      input: { stateId },
    });
    if (data.issueUpdate?.success !== true) throw new Error(refused);
  });
}

// --- issueAddLabel / issueRemoveLabel ----------------------------------------

const ISSUE_ADD_LABEL_MUTATION =
  "mutation($id: String!, $labelId: String!) { issueAddLabel(id: $id, labelId: $labelId) { success } }";

const ISSUE_REMOVE_LABEL_MUTATION =
  "mutation($id: String!, $labelId: String!) { issueRemoveLabel(id: $id, labelId: $labelId) { success } }";

/** Put one label on an issue, or take it off.
 *
 *  Linear's own two mutations rather than an `issueUpdate { labelIds }`. That input replaces
 *  the whole list, so using it would mean reading the list first and writing back a set that
 *  silently drops anything labelled in Linear in between. These name the one label they touch.
 *
 *  Both are idempotent, which is what keeps the retry across keys safe: adding a label twice
 *  is adding it once. */
export async function issueSetLabel(
  keys: string[],
  ticket: IssueRef,
  labelId: string,
  on: boolean,
): Promise<void> {
  if (labelId.trim() === "") {
    throw new Error(`that label carries no Linear id, so ${ticket.id} cannot be changed`);
  }
  const refused = `Linear refused the label change on ${ticket.id}`;
  await withFirstKey(keys, refused, async (key) => {
    const uuid = await issueUuid(key, ticket);
    const data = await gql<{
      issueAddLabel?: { success?: boolean };
      issueRemoveLabel?: { success?: boolean };
    }>(key, on ? ISSUE_ADD_LABEL_MUTATION : ISSUE_REMOVE_LABEL_MUTATION, {
      id: uuid,
      labelId,
    });
    const ok = on ? data.issueAddLabel?.success : data.issueRemoveLabel?.success;
    if (ok !== true) throw new Error(refused);
  });
}

/** The UUID a mutation addresses the issue by.
 *
 *  Every ticket the live query returns carries one. A ticket that does not (a fixture, or one
 *  built from the server's older wire shape) costs one lookup by identifier rather than a
 *  crash, and a lookup that finds nothing says which identifier failed. */
export async function issueUuid(key: string, ticket: IssueRef): Promise<string> {
  const known = (ticket.uuid ?? "").trim();
  if (known !== "") return known;

  const data = await gql<{ issue?: { id?: string } }>(key, ISSUE_UUID_QUERY, { id: ticket.id });
  const uuid = (data.issue?.id ?? "").trim();
  if (uuid === "") throw new Error(`Linear has no issue ${ticket.id} for this key`);
  return uuid;
}
