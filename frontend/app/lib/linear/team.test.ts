// What the panel's two menus are allowed to offer. Four rules decide it, and each one is wrong
// in a way that puts a row on a menu that a click could never apply: a key answering for
// somebody else's workspace, a label Linear has retired, one that arrived with no id, and a
// state whose kind this app has no glyph for.
import { expect, test } from "bun:test";

import { LABEL_FALLBACK_COLOR } from "./queries";
import {
  fetchTeamMeta,
  labelsFromResponse,
  sortLabels,
  statesFromResponse,
  teamKnown,
} from "./team";

/** An answer as the query shapes it: the team with its workflow, then the labels. */
function answer(teams: unknown[], labels: unknown[] = []) {
  return { teams: { nodes: teams }, issueLabels: { nodes: labels } };
}

/** One team node carrying `states`. */
function team(states: unknown[]) {
  return { id: "team-uuid", states: { nodes: states } };
}

// --- which key is believed ---------------------------------------------------
//
// The load-bearing one. Every key answers this query. A key for another workspace matches no
// team, but the workspace-wide half of the label filter still fills its answer with that
// account's own labels, and adding one of those to this ticket fails.

test("a key that cannot see the team is not believed", () => {
  expect(teamKnown(answer([], [{ id: "l1", name: "Bug" }]))).toBe(false);
  expect(teamKnown(answer([team([])]))).toBe(true);
});

test("an answer that is not the shape asked for is not believed either", () => {
  expect(teamKnown(null)).toBe(false);
  expect(teamKnown({ teams: null })).toBe(false);
  expect(teamKnown({ teams: { nodes: "some" } })).toBe(false);
});

// --- what reaches the label menu ---------------------------------------------

test("a retired label is left out, whatever else it carries", () => {
  const labels = [
    { id: "l1", name: "Bug", color: "#eb5757" },
    { id: "l2", name: "Old", color: "#000000", retiredAt: "2026-01-02T03:04:05.000Z" },
    { id: "l3", name: "Video", color: "#0f7488", retiredAt: null },
  ];

  expect(labelsFromResponse(answer([team([])], labels)).map((l) => l.name)).toEqual([
    "Bug",
    "Video",
  ]);
});

test("a label with no id or no name is dropped, since a click could not apply it", () => {
  const labels = [
    { id: "", name: "Nameless id", color: "#111111" },
    { id: "l2", name: "", color: "#222222" },
    { id: "l3", name: "Docs", color: "#5e6ad2" },
  ];

  expect(labelsFromResponse(answer([team([])], labels))).toEqual([
    { id: "l3", name: "Docs", color: "#5e6ad2" },
  ]);
});

test("a label with no colour of its own falls back to the same slate a ticket's does", () => {
  const labels = [{ id: "l1", name: "Bug", color: null }, { id: "l2", name: "Docs" }];

  expect(labelsFromResponse(answer([team([])], labels))).toEqual([
    { id: "l1", name: "Bug", color: LABEL_FALLBACK_COLOR },
    { id: "l2", name: "Docs", color: LABEL_FALLBACK_COLOR },
  ]);
});

test("an answer with no labels in it is an empty list rather than a throw", () => {
  expect(labelsFromResponse(answer([team([])], []))).toEqual([]);
  expect(labelsFromResponse(null)).toEqual([]);
  expect(labelsFromResponse({ issueLabels: { nodes: 7 } })).toEqual([]);
});

test("labels sort by name without case deciding it", () => {
  const labels = [
    { id: "l1", name: "video", color: "#0f7488" },
    { id: "l2", name: "Bug", color: "#eb5757" },
    { id: "l3", name: "bug-report", color: "#eb5757" },
  ];

  expect(sortLabels(labels).map((l) => l.name)).toEqual(["Bug", "bug-report", "video"]);
  // Linear's own order is by creation, so the answer arrives unsorted and is sorted here.
  expect(labelsFromResponse(answer([team([])], labels)).map((l) => l.name)).toEqual([
    "Bug",
    "bug-report",
    "video",
  ]);
});

// --- what reaches the state menu ---------------------------------------------
//
// The workspace's own names, in the workspace's own order. Linear stores `position` as one
// sequence across every kind, and its board reads left to right in it, so that is the order
// the menu has to be in for the two tools to agree.

test("states come back in the team's own order, with its own names", () => {
  const states = [
    { id: "s5", name: "Done", type: "completed", position: 3 },
    { id: "s2", name: "Todo", type: "unstarted", position: 1 },
    { id: "s3", name: "In Progress", type: "started", position: 2 },
    { id: "s1", name: "Icebox", type: "backlog", position: 0 },
    { id: "s6", name: "Duplicate", type: "duplicate", position: 5 },
    { id: "s4", name: "Canceled", type: "canceled", position: 4 },
  ];

  expect(statesFromResponse(answer([team(states)])).map((s) => s.name)).toEqual([
    "Icebox",
    "Todo",
    "In Progress",
    "Done",
    "Canceled",
    "Duplicate",
  ]);
});

/** The whole reason the menu lists states rather than kinds. By kind these two are one choice,
 *  and picking the second of them is a thing people do every day. */
test("two states of one kind are two rows, in position order", () => {
  const states = [
    { id: "review", name: "In Review", type: "started", position: 3 },
    { id: "doing", name: "In Progress", type: "started", position: 2 },
  ];

  expect(statesFromResponse(answer([team(states)]))).toEqual([
    { id: "doing", name: "In Progress", type: "in_progress" },
    { id: "review", name: "In Review", type: "in_progress" },
  ]);
});

test("a state with no position sorts last rather than first", () => {
  const states = [
    { id: "s2", name: "Unranked", type: "started" },
    { id: "s1", name: "Todo", type: "unstarted", position: 1 },
  ];

  expect(statesFromResponse(answer([team(states)])).map((s) => s.name)).toEqual([
    "Todo",
    "Unranked",
  ]);
});

test("a state of a kind we cannot draw is dropped, along with a nameless one", () => {
  const states = [
    { id: "s1", name: "Todo", type: "unstarted", position: 1 },
    { id: "s2", name: "Something New", type: "linear_invented_this", position: 2 },
    { id: "s3", name: "", type: "started", position: 3 },
    { id: "", name: "No id", type: "completed", position: 4 },
  ];

  expect(statesFromResponse(answer([team(states)])).map((s) => s.id)).toEqual(["s1"]);
});

test("an answer with no workflow in it is an empty list rather than a throw", () => {
  expect(statesFromResponse(answer([]))).toEqual([]);
  expect(statesFromResponse(null)).toEqual([]);
  expect(statesFromResponse({ teams: { nodes: [{ states: 7 }] } })).toEqual([]);
});

// No request is made, which is what makes this safe to run.
test("asking with no key says so rather than reaching Linear", async () => {
  await expect(fetchTeamMeta("", "WE")).rejects.toThrow(/no Linear API key configured/);
});
