// Who a new ticket can be assigned to: the members of one Linear team.
//
// One round trip answers both halves of the question, because the dialog needs them together:
// the team's members are the options, and the viewer is which of them is "me". Asking twice
// would be two chances for one to be the slow one, and a moment where the list is known but
// the default is not.
//
// Membership is per team, so the list is refetched when the team changes. That is not a cache
// worth building: a workspace has tens of members, not thousands, and the answer is only ever
// needed while the dialog is open.
//
// The mapping is pure and exported, like every other reader of a Linear response, because the
// interesting rules are there: who is unusable, who is dropped, and what order they come in.

import { gql } from "~/lib/linear/client";

/** Somebody a ticket can be assigned to. */
export interface TicketPerson {
  /** Linear's user UUID, which is what `issueCreate` takes. */
  id: string;
  /** Their full name, or the email when Linear has no name for them. */
  name: string;
  email: string;
  /** They are the key's own owner. The dialog marks them and starts on them. */
  isViewer: boolean;
}

/** A team's members and the key owner's own id, in one query. `active` is asked for because a
 *  deactivated member is still a member and Linear will not accept the assignment. */
export const TEAM_PEOPLE_QUERY =
  "query($team: String!) { " +
  "teams(filter: { key: { eq: $team } }, first: 1) { " +
  "nodes { members(first: 100) { nodes { id name displayName email active } } } } " +
  "viewer { id } }";

/** Everyone in the team who can hold a ticket, the viewer first and the rest by name.
 *
 *  Viewer-first is the order the dialog reads in: the default sits at the top of its own list
 *  rather than wherever the alphabet put it. A member with no id, or a deactivated one, is
 *  dropped: offering somebody Linear will refuse is worse than a shorter list. */
export function peopleFromResponse(data: unknown): TicketPerson[] {
  const root = record(data);
  const teamNode = record(list(record(root?.teams)?.nodes)[0]);
  const viewerId = str(record(root?.viewer), "id");

  const people: TicketPerson[] = [];
  for (const value of list(record(teamNode?.members)?.nodes)) {
    const node = record(value);
    if (!node) continue;
    if (node.active === false) continue;
    const id = str(node, "id");
    if (id === "") continue;
    const email = str(node, "email");
    const name = str(node, "name") || str(node, "displayName") || email;
    if (name === "") continue;
    people.push({ id, name, email, isViewer: id === viewerId && viewerId !== "" });
  }

  return people.sort((a, b) => {
    if (a.isViewer !== b.isViewer) return a.isViewer ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
}

/** The team's assignable members. Throws whatever `gql` throws, which the dialog reports as
 *  the reason the picker is empty rather than as a failed create. */
export async function fetchTeamPeople(key: string, team: string): Promise<TicketPerson[]> {
  if (key.trim() === "") throw new Error("no Linear API key configured for that workspace");
  const data = await gql<unknown>(key, TEAM_PEOPLE_QUERY, { team: team.trim().toUpperCase() });
  return peopleFromResponse(data);
}

/** Who the dialog starts on: the viewer, or the first member when the key owns no personal
 *  user (an app or OAuth actor). Blank when nobody can be assigned, which leaves the create
 *  to fall back to the key's own owner. */
export function defaultAssignee(people: TicketPerson[]): string {
  return (people.find((p) => p.isViewer) ?? people[0])?.id ?? "";
}

// --- reading an answer nothing type-checked ----------------------------------

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function str(node: Record<string, unknown> | null, key: string): string {
  const value = node?.[key];
  return typeof value === "string" ? value : "";
}
