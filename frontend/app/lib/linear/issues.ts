// One Linear issue, looked up by the identifier an operator typed, and moved to In Progress.
//
// Two callers, one lookup. The clone dialog resolves what was typed into it and then moves the
// issue; the board resolves the ticket behind a clone that already exists, so a card and its
// panel draw Linear's own title rather than the one copied onto the clone when it was made.
// Both filter the same way and differ only in the field set they ask for.
//
// The move to In Progress is the last of the Linear calls the clone route made server-side. The
// browser holds every preset key now, so it does both itself and posts the answer to
// `/api/clone`. What crossed over is the part the function names do not say:
//
// - The lookup filters by team key AND issue number, not by the `WE-142` string. Linear's
//   `issues(filter:)` has no identifier field, so the id is split and both halves are sent.
// - It is tried against every configured key in turn and the first that sees the issue wins.
//   The key that saw it is handed back, because that is the one proven to be allowed to write
//   it, and the state change that follows is a write.
// - The state change prefers a state literally NAMED "In Progress" over any state merely of
//   type `started`. A team with both "In Progress" and "In Review" started states would
//   otherwise get whichever Linear listed first.
// - It never drags an issue backwards. An issue already in a started state is left alone,
//   which is what keeps a second clone off the same ticket from resetting its column.
//
// Everything that decides a value is pure and exported. The two network calls are thin
// wrappers over those, so the rules are testable without a key.

import type { CloneLinearMeta } from "~/lib/api";
import { gql } from "~/lib/linear/client";
import { OPEN_ISSUE_FIELDS, ticketFromNode } from "~/lib/linear/queries";
import type { LinearTicket, TicketState } from "~/lib/linear/types";
import { parseTicketInput } from "~/lib/workspace";

/** A Linear issue as the clone route needs it: what a clone stores, plus the workflow state
 *  the move decision reads and the UUID a mutation addresses.
 *
 *  Deliberately not `LinearTicket`. That shape is what the ticket column draws, and it maps
 *  Linear's state *type* onto a five-value union that drops anything it does not model. Here
 *  the raw type is kept verbatim, so an issue sitting in `triage` resolves and clones instead
 *  of reading as missing. */
export interface ResolvedIssue {
  /** Lowercase team key, e.g. `we`. */
  prefix: string;
  /** Linear's own UUID. Empty when the answer carried none. */
  uuid: string;
  /** Human identifier, e.g. `WE-142`, exactly as Linear spells it. */
  identifier: string;
  title: string;
  url: string;
  /** Linear's own `branchName`. */
  branch: string;
  /** Linear's state *type*, verbatim: `unstarted`, `started`, `backlog`, `completed`,
   *  `canceled`, `triage`. */
  stateType: string;
  /** Every label name, in Linear's order. The first one is what a clone stores. */
  labels: string[];
}

/** The team key and number a lookup is filtered by, plus the identifier they spell. */
export interface IssueRef {
  /** Uppercase team key, e.g. `WE`, which is how Linear stores it. */
  teamKey: string;
  number: number;
  /** `WE-142`. */
  identifier: string;
  /** Lowercase team key, e.g. `we`. What picks the preset. */
  prefix: string;
}

/** A `WE-142`-style reference out of a pasted Linear link or a bare id, or null when the input
 *  holds neither.
 *
 *  `parseTicketInput` does the finding, so the dialog's own preview and this lookup can never
 *  disagree about which id was typed. All this adds is the split into the two halves the query
 *  filters by, which Linear needs because its issue filter has no identifier field. */
export function issueRefOf(input: string): IssueRef | null {
  const parsed = parseTicketInput(input);
  if (!parsed) return null;
  const dash = parsed.identifier.lastIndexOf("-");
  const number = Number(parsed.identifier.slice(dash + 1));
  if (!Number.isSafeInteger(number)) return null;
  return {
    teamKey: parsed.identifier.slice(0, dash),
    number,
    identifier: parsed.identifier,
    prefix: parsed.prefix,
  };
}

// --- looking one up ----------------------------------------------------------

/** What a clone needs off an issue. Narrower than the column's field set: no parent, no
 *  children, no priority, because none of them reaches a clone. */
const ISSUE_FIELDS =
  "id identifier title url branchName state { id name type } labels { nodes { name } }";

/** By team key and number, because Linear's issue filter has no identifier field. `$num` is a
 *  `Float!`, which is what its schema declares for an issue number.
 *
 *  No state filter, unlike the open-issues query. A clone outlives its ticket being marked
 *  Done, and the ticket is still the thing the clone is named after, so every state answers. */
export function issueByNumberQuery(fields: string): string {
  return (
    "query($team: String!, $num: Float!) { " +
    "issues(filter: { team: { key: { eq: $team } }, number: { eq: $num } }, first: 1) { " +
    `nodes { ${fields} } } }`
  );
}

const ISSUE_BY_NUMBER_QUERY = issueByNumberQuery(ISSUE_FIELDS);

/** The same lookup asking for the field set the ticket column and its panel draw, so a clone's
 *  ticket arrives in the shape every ticket-rendering component already takes. */
const TICKET_BY_NUMBER_QUERY = issueByNumberQuery(OPEN_ISSUE_FIELDS);

/** One `ISSUE_FIELDS` node as a `ResolvedIssue`, or null when it carries no identifier.
 *
 *  Every other field falls back rather than throwing: an issue with no branch name or no
 *  labels is a real issue, and a clone made from it should differ in those fields and nowhere
 *  else. `prefix` comes from the caller's own reference rather than the answer, which is what
 *  keeps it lowercase and matching the preset labels. */
export function issueFromNode(prefix: string, value: unknown): ResolvedIssue | null {
  const node = obj(value);
  if (!node) return null;
  const identifier = str(node, "identifier");
  if (identifier === "") return null;
  return {
    prefix,
    uuid: str(node, "id"),
    identifier,
    title: str(node, "title"),
    url: str(node, "url"),
    branch: str(node, "branchName"),
    stateType: str(obj(node.state) ?? {}, "type"),
    labels: names(obj(node.labels)?.nodes),
  };
}

/** The issue `ref` names, read with one key. Throws the sentence a banner shows when the key
 *  cannot see it, which is what makes `fetchIssueAny` able to move on to the next one. */
export async function fetchIssue(key: string, ref: IssueRef): Promise<ResolvedIssue> {
  const data = await gql<{ issues?: { nodes?: unknown[] } }>(key, ISSUE_BY_NUMBER_QUERY, {
    team: ref.teamKey,
    num: ref.number,
  });
  const issue = issueFromNode(ref.prefix, data.issues?.nodes?.[0]);
  if (!issue) throw new Error(`ticket ${ref.identifier} not found in Linear`);
  return issue;
}

/** The issue `ref` names, as the ticket column's own shape. Same filter and same throw as
 *  `fetchIssue`; the field set is the wider one every ticket component draws from.
 *
 *  A state the column does not model (`triage`, or anything Linear adds later) reads as a miss
 *  rather than as a ticket, because `ticketFromNode` refuses it. */
export async function fetchTicket(key: string, ref: IssueRef): Promise<LinearTicket> {
  const data = await gql<{ issues?: { nodes?: unknown[] } }>(key, TICKET_BY_NUMBER_QUERY, {
    team: ref.teamKey,
    num: ref.number,
  });
  const ticket = ticketFromNode(data.issues?.nodes?.[0]);
  if (!ticket) throw new Error(`ticket ${ref.identifier} not found in Linear`);
  return ticket;
}

/** Run `load` against each key in turn until one answers, and hand back the key that did.
 *
 *  Every key is personal and answers only for its own workspace, so a fleet pointing at two
 *  Linear accounts has to ask both. `keysForTeam` orders them so the preset claiming the team
 *  goes first, which means the usual fleet spends exactly one request here.
 *
 *  The key comes back with the answer. It is the one proven to reach the issue, so it is the
 *  one a write that follows uses. */
async function firstKeyThatSees<T>(
  keys: string[],
  load: (key: string) => Promise<T>,
): Promise<{ value: T; key: string }> {
  let last: Error | null = null;
  const seen = new Set<string>();
  for (const raw of keys) {
    const key = raw.trim();
    if (key === "" || seen.has(key)) continue;
    seen.add(key);
    try {
      return { value: await load(key), key };
    } catch (e) {
      last = e as Error;
    }
  }
  throw (
    last ?? new Error("no preset has a Linear API key configured. Add one in Settings")
  );
}

/** The issue `ref` names, trying each key in turn until one sees it. The key that found it
 *  comes back with it, because `ensureInProgress` writes with that one. */
export async function fetchIssueAny(
  keys: string[],
  ref: IssueRef,
): Promise<{ issue: ResolvedIssue; key: string }> {
  const found = await firstKeyThatSees(keys, (key) => fetchIssue(key, ref));
  return { issue: found.value, key: found.key };
}

/** The ticket `ref` names, trying each key in turn until one sees it. Nothing writes after
 *  this, so the key that answered is dropped. */
export async function fetchTicketAny(keys: string[], ref: IssueRef): Promise<LinearTicket> {
  return (await firstKeyThatSees(keys, (key) => fetchTicket(key, ref))).value;
}

// --- moving it to In Progress ------------------------------------------------

const TEAM_STATES_QUERY =
  "query($team: String!) { teams(filter: { key: { eq: $team } }, first: 1) { " +
  "nodes { states { nodes { id name type } } } } }";

const SET_STATE_MUTATION =
  "mutation($id: String!, $state: String!) { " +
  "issueUpdate(id: $id, input: { stateId: $state }) { success } }";

/** The state an issue is moved into, out of a team's whole workflow.
 *
 *  A state literally named "In Progress" wins. Only if the team has none does any state of
 *  type `started` do, and then the first Linear listed. The distinction is real: a team with
 *  "In Progress" and "In Review" has two started states, and landing a fresh clone in review
 *  is wrong in a way nobody would notice until the standup.
 *
 *  Null when the team declares no started state at all, which the caller reports rather than
 *  guessing at.
 *
 *  Takes the raw `states.nodes` array, because nothing type-checked the answer. */
export function pickInProgressStateId(nodes: unknown): string | null {
  if (!Array.isArray(nodes)) return null;
  const states = nodes.map(obj).filter((n): n is Json => n !== null);
  const named = states.find((s) => str(s, "name") === "In Progress");
  const started = named ?? states.find((s) => str(s, "type") === "started");
  const id = started === undefined ? "" : str(started, "id");
  return id === "" ? null : id;
}

/** Whether an issue in `stateType` should be moved at all.
 *
 *  An issue already in a started state is left where it is. Two clones off one ticket is a
 *  normal thing to do, and the second must not drag the ticket back out of review to say so.
 *  Every other state moves forward, cancelled and completed included: cloning a ticket is
 *  picking it back up. */
export function needsInProgress(stateType: string): boolean {
  return stateType !== "started";
}

/** Move an issue to In Progress unless it is already started.
 *
 *  Best effort by contract, the same as the server call it replaces, which only warned. The
 *  clone is the thing the operator asked for, and a workflow column is not worth failing it
 *  over, so callers run this and swallow what it throws.
 *
 *  Answers whether it wrote anything, which is what makes "it refused to drag it backwards"
 *  observable without reading Linear again. */
export async function ensureInProgress(key: string, issue: ResolvedIssue): Promise<boolean> {
  if (!needsInProgress(issue.stateType)) return false;
  if (issue.uuid === "") throw new Error(`${issue.identifier} arrived without a Linear id`);

  const team = issue.prefix.toUpperCase();
  const data = await gql<{ teams?: { nodes?: unknown[] } }>(key, TEAM_STATES_QUERY, { team });
  const node = obj(data.teams?.nodes?.[0]);
  const stateId = pickInProgressStateId(obj(node?.states)?.nodes);
  if (stateId === null) throw new Error(`no "In Progress" state found for team ${team}`);

  const updated = await gql<{ issueUpdate?: { success?: boolean } }>(key, SET_STATE_MUTATION, {
    id: issue.uuid,
    state: stateId,
  });
  if (updated.issueUpdate?.success !== true) {
    throw new Error(`failed to move ${issue.identifier} to In Progress`);
  }
  return true;
}

// --- handing it to the clone route -------------------------------------------

/** The `linear` object `POST /api/clone` takes, off a resolved issue.
 *
 *  Every field is Linear's own answer rather than anything the operator typed, which is what
 *  makes the clone's stored title, url and branch match the ticket they name. `label` is the
 *  first label only, because that is the single one a clone carries. */
export function cloneLinearMeta(issue: ResolvedIssue): CloneLinearMeta {
  const meta: CloneLinearMeta = {
    workspace: issue.prefix,
    ticket: issue.identifier,
    ticketUrl: issue.url,
    branch: issue.branch,
    title: issue.title,
  };
  const label = issue.labels[0];
  if (label !== undefined && label !== "") meta.label = label;
  return meta;
}

/** Linear's own state type for one of ours. The inverse of `stateOf` in `queries.ts`.
 *
 *  Needed because a newly opened issue arrives as a `LinearTicket`, whose state has already
 *  been mapped, and the move decision reads the raw type. `todo` answers `unstarted` and not
 *  the other way round, so a fresh issue is correctly seen as not yet started. */
export function stateTypeOf(state: TicketState): string {
  switch (state) {
    case "backlog":
      return "backlog";
    case "todo":
      return "unstarted";
    case "in_progress":
      return "started";
    case "done":
      return "completed";
    case "canceled":
      return "canceled";
  }
}

/** A ticket `issueCreate` just answered with, as a resolved issue.
 *
 *  The create path opens the issue and then moves it, exactly as the server did, so the ticket
 *  Linear hands back has to reach the same two functions the looked-up one does. `prefix` falls
 *  back to the identifier's own team part, because `team` is the only field of the two that an
 *  answer can omit. */
export function resolvedFromTicket(ticket: LinearTicket): ResolvedIssue {
  const dash = ticket.id.lastIndexOf("-");
  const fromId = dash > 0 ? ticket.id.slice(0, dash) : ticket.id;
  return {
    prefix: (ticket.team ?? fromId).toLowerCase(),
    uuid: ticket.uuid ?? "",
    identifier: ticket.id,
    title: ticket.title,
    url: ticket.url,
    branch: ticket.branchName ?? "",
    stateType: stateTypeOf(ticket.state),
    labels: ticket.labels.map((l) => l.name),
  };
}

// --- reading an answer nothing type-checked ----------------------------------

type Json = Record<string, unknown>;

function obj(value: unknown): Json | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Json)
    : null;
}

function str(node: Json, key: string): string {
  const value = node[key];
  return typeof value === "string" ? value : "";
}

/** The `name` of every node in a `{ nodes: [...] }` list that has one. */
function names(nodes: unknown): string[] {
  if (!Array.isArray(nodes)) return [];
  const kept: string[] = [];
  for (const node of nodes) {
    const name = obj(node);
    if (name && typeof name.name === "string" && name.name !== "") kept.push(name.name);
  }
  return kept;
}
