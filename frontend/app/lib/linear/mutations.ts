// The two writes the ticket column can start: open an issue, and edit one.
//
// Both used to be the server's. The browser holds the preset keys now, so it addresses Linear
// itself and the control-server never sees a ticket body. What crossed over unchanged is the
// part that is not obvious from the mutation names:
//
// - A new issue is pinned to the team's lowest-`position` `unstarted` state. The column draws
//   Todo and In Progress only, so an issue left in whatever state the team defaults to (very
//   often Backlog) would be created and disappear in the same breath.
// - A new issue is assigned to the key's own owner, because the column lists what that owner
//   is assigned. An unassigned issue would never appear in it either.
// - An edit addresses the issue by Linear's UUID, never by `WE-142`.
//
// Everything that decides a variable is pure and exported. The network calls below are thin
// wrappers over those, so the rules are testable without a key.

import { gql } from "~/lib/linear/client";
import { OPEN_ISSUE_FIELDS, ticketFromNode } from "~/lib/linear/queries";
import type { LinearTicket } from "~/lib/linear/types";

/** What the new-ticket dialog sends. `priority` is Linear's own scale: 1 urgent, 2 high,
 *  3 medium, 4 low. Zero and absent both mean no priority, which is a real value there. */
export interface NewIssue {
  /** Team key, e.g. `WE`. Also what picks the key that opens it. */
  team: string;
  title: string;
  description: string;
  priority?: number;
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

/** The state a new issue is pinned to: the lowest-`position` state of type `unstarted`.
 *
 *  A team can have several Todo states, and Linear orders them by `position`, so the lowest
 *  is the one its own board drops a new issue into. Null when the team declares none, which
 *  leaves the choice to Linear rather than inventing one.
 *
 *  Takes the raw `states.nodes` array, because nothing type-checked the answer. */
export function pickCreateStateId(nodes: unknown): string | null {
  if (!Array.isArray(nodes)) return null;
  const unstarted = nodes.filter(
    (n) => typeof n === "object" && n !== null && (n as { type?: unknown }).type === "unstarted",
  ) as { id?: unknown; position?: unknown }[];
  // Stable, so two states sharing a position keep the order Linear listed them in. A state
  // with no position sorts last rather than first: an unknown rank is not rank zero.
  const ranked = [...unstarted].sort((a, b) => rank(a.position) - rank(b.position));
  const id = ranked[0]?.id;
  return typeof id === "string" && id !== "" ? id : null;
}

function rank(position: unknown): number {
  return typeof position === "number" && Number.isFinite(position) ? position : Number.MAX_VALUE;
}

/** The `IssueCreateInput` for a new issue.
 *
 *  `assigneeId` is left off rather than sent as null when the key has no personal user, which
 *  is the case for an app or OAuth actor. Creation still works. The issue is then unassigned
 *  and the column will not list it. Same for `stateId`. */
export function issueCreateInput(fields: {
  teamId: string;
  title: string;
  description: string;
  viewerId?: string | null;
  stateId?: string | null;
  priority?: number;
}): Record<string, unknown> {
  const input: Record<string, unknown> = {
    teamId: fields.teamId,
    title: fields.title,
    description: fields.description,
  };
  if (fields.viewerId) input.assigneeId = fields.viewerId;
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

/** Write a title and/or description onto an issue.
 *
 *  `keys` is tried in order and the first that lands wins, mirroring the server's rule that a
 *  key which can see an issue is the key that may write it. A workspace's own key is put
 *  first by `keysForTeam`, so the usual fleet spends exactly one request here.
 *
 *  Re-trying a patch against a second key cannot double-apply it: the patch is the value to
 *  set, not a delta, so applying it twice is applying it once. */
export async function issueUpdate(
  keys: string[],
  ticket: IssueRef,
  patch: IssuePatch,
): Promise<void> {
  const input = issueUpdateInput(patch);
  if (Object.keys(input).length === 0) return;
  if (keys.length === 0) throw new Error("no Linear API key configured for that workspace");

  let last: Error | null = null;
  for (const key of keys) {
    try {
      const uuid = await issueUuid(key, ticket);
      const data = await gql<{ issueUpdate?: { success?: boolean } }>(
        key,
        ISSUE_UPDATE_MUTATION,
        { id: uuid, input },
      );
      if (data.issueUpdate?.success !== true) {
        throw new Error(`Linear refused the update to ${ticket.id}`);
      }
      return;
    } catch (e) {
      last = e as Error;
    }
  }
  throw last ?? new Error(`Linear refused the update to ${ticket.id}`);
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
