// What one Linear team offers a ticket in it: the labels it can carry, and the workflow states
// it can be moved into.
//
// Both in one query, because both are properties of the same team and two requests would be two
// chances for one of them to be the slow one. The panel opens both menus off one answer.
//
// The team itself is asked for first, and that half is a check rather than data. Each preset key
// answers for its own workspace, and a key pointing somewhere else still answers this query: its
// team filter matches nothing, but the workspace-wide half of the label filter comes back full of
// labels that belong to a different Linear account. So a key that cannot see the team is refused
// rather than believed.
//
// Label groups are filtered out at the source. A group is a container Linear draws in its own
// picker and refuses on an issue, so offering one would offer a click that always fails.
//
// Everything below the fetch is pure and exported, because the rules are what is worth testing:
// which key is trusted, what a retired label does, and what order the two menus read in.

import { gql } from "~/lib/linear/client";
import { LABEL_FALLBACK_COLOR, stateFromType } from "~/lib/linear/queries";
import type { TicketLabel, TicketWorkflowState } from "~/lib/linear/types";

/** How many labels one answer carries. Linear caps a page at 250 and this asks for the lot:
 *  a workspace past that has more labels than a dropdown could be read in anyway, and the
 *  overflow is lost silently, the same trade the open-issues query makes. */
export const LABEL_PAGE = 250;

/** What a team offers the ticket open in the panel. */
export interface TeamMeta {
  labels: TicketLabel[];
  states: TicketWorkflowState[];
}

/** The team with its workflow, then the labels anything in it can carry.
 *
 *  `retiredAt` rides along on both because neither filter has a field for it, so a retired one
 *  has to be dropped here instead of by Linear. `position` is what puts the states in the order
 *  the team's own board reads in. */
export const TEAM_META_QUERY =
  "query($team: String!) { " +
  "teams(filter: { key: { eq: $team } }, first: 1) { nodes { id " +
  "states { nodes { id name type position } } } } " +
  `issueLabels(first: ${LABEL_PAGE}, filter: { isGroup: { eq: false }, ` +
  "or: [{ team: { null: true } }, { team: { key: { eq: $team } } }] }" +
  ") { nodes { id name color retiredAt } } }";

/** Whether the key that answered can see the team at all.
 *
 *  False means the labels in the same answer belong to somebody else's workspace, so the
 *  caller moves on to the next key rather than offering them. */
export function teamKnown(data: unknown): boolean {
  const nodes = obj(obj(data)?.teams)?.nodes;
  return Array.isArray(nodes) && nodes.length > 0;
}

/** Every usable label in an answer, sorted by name.
 *
 *  Retired labels are dropped: Linear keeps them on the issues that already carry them and
 *  offers them nowhere new, which is exactly what this list is for. A label with no id is
 *  dropped too, since adding it would name nothing.
 *
 *  Sorted rather than left in Linear's order, which is by creation, because this is a list
 *  somebody reads down looking for one name. */
export function labelsFromResponse(data: unknown): TicketLabel[] {
  const nodes = obj(obj(data)?.issueLabels)?.nodes;
  if (!Array.isArray(nodes)) return [];
  const kept: TicketLabel[] = [];
  for (const value of nodes) {
    const node = obj(value);
    if (!node) continue;
    if (node.retiredAt !== null && node.retiredAt !== undefined) continue;
    const id = str(node, "id");
    const name = str(node, "name");
    if (id === "" || name === "") continue;
    kept.push({ id, name, color: str(node, "color") || LABEL_FALLBACK_COLOR });
  }
  return sortLabels(kept);
}

/** By name, case-insensitively, so `bug` and `Bug` do not sit at opposite ends of the menu. */
export function sortLabels(labels: TicketLabel[]): TicketLabel[] {
  return [...labels].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
}

/** The team's workflow, in `position` order.
 *
 *  That order is the team's own: Linear stores it as one sequence across every kind, and its
 *  board reads left to right in it. Sorting by anything else here would put the menu in a
 *  different order from the tool the operator sets these in every other day.
 *
 *  A state whose type this app does not model is dropped rather than drawn without a glyph.
 *  Every type Linear has today is modelled, so that is a state type it adds later. */
export function statesFromResponse(data: unknown): TicketWorkflowState[] {
  const teams = obj(obj(data)?.teams)?.nodes;
  const nodes = Array.isArray(teams) ? obj(obj(teams[0])?.states)?.nodes : null;
  if (!Array.isArray(nodes)) return [];
  const kept: { state: TicketWorkflowState; position: number }[] = [];
  for (const value of nodes) {
    const state = obj(value);
    if (!state) continue;
    const id = str(state, "id");
    const name = str(state, "name");
    const type = stateFromType(str(state, "type"));
    if (id === "" || name === "" || type === null) continue;
    kept.push({ state: { id, name, type }, position: rank(state.position) });
  }
  // Stable, so two states sharing a position keep the order Linear listed them in, and one
  // with no position at all sorts last rather than first.
  return kept.sort((a, b) => a.position - b.position).map((k) => k.state);
}

function rank(position: unknown): number {
  return typeof position === "number" && Number.isFinite(position) ? position : Number.MAX_VALUE;
}

/** What team `team` offers, read with one key.
 *
 *  Throws when the key cannot see the team, which is what makes `teamMeta` able to move on to
 *  the next one. */
export async function fetchTeamMeta(key: string, team: string): Promise<TeamMeta> {
  const data = await gql<unknown>(key, TEAM_META_QUERY, { team: team.toUpperCase() });
  if (!teamKnown(data)) throw new Error(`this Linear key cannot see team ${team}`);
  return { labels: labelsFromResponse(data), states: statesFromResponse(data) };
}

/** What has already been asked, by team and key list.
 *
 *  A team's labels and workflow change about as often as its name does, and two panels open on
 *  one team would otherwise be two requests for one answer. Nothing clears this, so a label or
 *  a state created in Linear mid-session appears after a reload. That is the trade: a menu that
 *  is a few minutes stale, against a request every time it opens.
 *
 *  A failed lookup is forgotten rather than kept, so a key typed into Settings after a failure
 *  gets a fresh attempt. */
const asked = new Map<string, Promise<TeamMeta>>();

/** What team `team` offers, from the first key that can see it.
 *
 *  Keys come in `keysForTeam` order, so the preset claiming the team is asked first and the
 *  usual fleet spends exactly one request. */
export function teamMeta(keys: string[], team: string): Promise<TeamMeta> {
  const at = [team, ...keys].join(" ");
  const hit = asked.get(at);
  if (hit) return hit;
  const pending = firstKeyThatSees(keys, team);
  asked.set(at, pending);
  // Handled here so a rejection nothing else awaited is not an unhandled one. Callers still
  // see the rejection: this branch is a second subscriber, not a replacement.
  pending.catch(() => asked.delete(at));
  return pending;
}

async function firstKeyThatSees(keys: string[], team: string): Promise<TeamMeta> {
  let last: Error | null = null;
  for (const key of keys) {
    try {
      return await fetchTeamMeta(key, team);
    } catch (e) {
      last = e as Error;
    }
  }
  throw last ?? new Error("no preset has a Linear API key configured. Add one in Settings");
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
