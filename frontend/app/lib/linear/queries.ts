// The open-issues query and the mapping from Linear's answer to a `LinearTicket`.
//
// The query is scoped through `viewer`, so a key answers for whoever owns it and nobody else.
// A fleet with several keys therefore gets several people's queues, which is the point: they
// are all the operator's own accounts.
//
// Everything below the fetch is pure and exported, because the interesting rules live in the
// mapping: what makes an issue unshowable, what a missing field falls back to. Those are
// testable without a network or a React tree, and they are what the tests cover.

import { gql } from "~/lib/linear/client";
import type { LinearTicket, TicketLabel, TicketLink, TicketState } from "~/lib/linear/types";

/** What a label with no colour of its own draws as: slate-400, the same grey the rest of the
 *  muted UI uses. Linear sends `null` for such a label, and a blank string is read the same
 *  way, because an empty colour is not a colour. */
export const LABEL_FALLBACK_COLOR = "#94a3b8";

/** The fields the ticket column and its panel draw.
 *
 *  `id` is Linear's UUID and `identifier` is `WE-142`. Both are asked for: the identifier is
 *  what the operator sees and types, the UUID is what a mutation addresses. */
const OPEN_ISSUE_FIELDS =
  "id identifier title url branchName priority description " +
  "team { key } state { type } labels { nodes { name color } } " +
  "parent { identifier title url state { type } } " +
  "children { nodes { identifier title url state { type } } }";

/** Every open issue assigned to the key's own owner, newest first.
 *
 *  "Open" is Linear's state *types* `unstarted` and `started`, which its UI shows as Todo and
 *  In Progress. Backlog, completed, cancelled and triage are excluded by the query rather
 *  than after the fact, so a workspace with thousands of closed issues costs nothing to poll.
 *
 *  `first: 100` with no pagination: an operator with more than 100 open assigned issues loses
 *  the overflow silently. Known, and left alone. */
export const OPEN_ISSUES_QUERY =
  "query { viewer { assignedIssues( " +
  'filter: { state: { type: { in: ["unstarted", "started"] } } }, ' +
  "orderBy: updatedAt, first: 100 " +
  `) { nodes { ${OPEN_ISSUE_FIELDS} } } } }`;

/** One key's open assigned issues. Throws the sentence a banner shows when the key, the
 *  network, or Linear itself refuses. */
export async function fetchOpenTickets(key: string): Promise<LinearTicket[]> {
  return ticketsFromResponse(await gql<unknown>(key, OPEN_ISSUES_QUERY));
}

/** Every mappable issue in an `OPEN_ISSUES_QUERY` answer, in the order Linear sent them.
 *
 *  A malformed or unrecognised node is dropped rather than defaulted, so a response Linear
 *  changes the shape of loses rows instead of drawing wrong ones. */
export function ticketsFromResponse(data: unknown): LinearTicket[] {
  const issues = obj(obj(obj(data)?.viewer)?.assignedIssues);
  return mapNodes(issues?.nodes, ticketFromNode);
}

/** One GraphQL node as a ticket, or null when it carries no identifier or a state type we do
 *  not model. */
export function ticketFromNode(value: unknown): LinearTicket | null {
  const node = obj(value);
  if (!node) return null;
  const id = str(node, "identifier");
  if (id === "") return null;
  const state = stateOf(node);
  if (state === null) return null;

  const ticket: LinearTicket = {
    id,
    title: str(node, "title"),
    url: str(node, "url"),
    state,
    labels: mapNodes(obj(node.labels)?.nodes, labelFromNode),
    children: mapNodes(obj(node.children)?.nodes, linkFromNode),
  };

  const uuid = optStr(node, "id");
  if (uuid !== undefined) ticket.uuid = uuid;
  const team = obj(node.team);
  if (team && typeof team.key === "string") ticket.team = team.key;
  // Linear sends `0` for "no priority", and 1 to 4 for urgent through low. Zero is absence,
  // not a rank, so it becomes an absent field rather than a badge reading "0".
  const priority = node.priority;
  if (typeof priority === "number" && Number.isInteger(priority) && priority >= 1 && priority <= 4) {
    ticket.priority = priority;
  }
  const branchName = optStr(node, "branchName");
  if (branchName !== undefined) ticket.branchName = branchName;
  const description = optStr(node, "description");
  if (description !== undefined) ticket.description = description;
  const parent = linkFromNode(node.parent);
  if (parent) ticket.parent = parent;
  return ticket;
}

/** A parent or sub-issue node as a link row. Any state qualifies here, unlike the list query:
 *  a sub-issue that is already done still belongs on its parent's panel. */
export function linkFromNode(value: unknown): TicketLink | null {
  const node = obj(value);
  if (!node) return null;
  const id = str(node, "identifier");
  if (id === "") return null;
  const state = stateOf(node);
  if (state === null) return null;
  return { id, title: str(node, "title"), url: str(node, "url"), state };
}

/** Linear's state *type* as ours. `triage`, and anything Linear adds later, answer null,
 *  which drops the issue rather than defaulting it: a ticket quietly showing up as Todo
 *  because its state was unrecognised is worse than one that does not show up at all. */
export function stateOf(value: unknown): TicketState | null {
  const state = obj(obj(value)?.state);
  switch (state === null ? "" : str(state, "type")) {
    case "backlog":
      return "backlog";
    case "unstarted":
      return "todo";
    case "started":
      return "in_progress";
    case "completed":
      return "done";
    case "canceled":
      return "canceled";
    default:
      return null;
  }
}

/** One label node, or null when it carries no name. */
function labelFromNode(value: unknown): TicketLabel | null {
  const node = obj(value);
  if (!node || typeof node.name !== "string") return null;
  return { name: node.name, color: optStr(node, "color") ?? LABEL_FALLBACK_COLOR };
}

// --- reading an answer nothing type-checked ----------------------------------
//
// The response is `unknown` all the way down, so every read below states what it expects and
// falls back rather than throwing. A field Linear renames costs one value, not the poll.

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

/** A string field, absent when it is missing or blank. `branchName` and `description` come
 *  back as `null` on an issue that has neither, and an empty string is the same nothing. */
function optStr(node: Json, key: string): string | undefined {
  const value = str(node, key);
  return value === "" ? undefined : value;
}

/** Map a `{ nodes: [...] }` list, dropping everything `map` refuses. */
function mapNodes<T>(nodes: unknown, map: (value: unknown) => T | null): T[] {
  if (!Array.isArray(nodes)) return [];
  const kept: T[] = [];
  for (const node of nodes) {
    const mapped = map(node);
    if (mapped !== null) kept.push(mapped);
  }
  return kept;
}
