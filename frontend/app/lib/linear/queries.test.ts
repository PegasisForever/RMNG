// Mapping Linear's answer is where a wrong ticket comes from, so the rules that refuse a node
// are the ones under test: an unrecognised state drops the issue rather than defaulting it,
// and a field Linear left empty becomes an absent field rather than an empty one.
import { expect, test } from "bun:test";

import {
  LABEL_FALLBACK_COLOR,
  OPEN_ISSUES_QUERY,
  linkFromNode,
  stateOf,
  ticketFromNode,
  ticketsFromResponse,
} from "./queries";

/** One `assignedIssues` node as Linear sends it: every field the query asks for, with the
 *  nulls a freshly filed issue really carries. */
function node(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: "3f2c1b4a-0d9e-4a11-9f3b-5c6d7e8f9012",
    identifier: "WE-142",
    title: "Encoder drops frames",
    url: "https://linear.app/pegasis/issue/WE-142/encoder-drops-frames",
    branchName: null,
    priority: 0,
    description: null,
    team: { key: "WE" },
    state: { type: "unstarted" },
    labels: { nodes: [] },
    parent: null,
    children: { nodes: [] },
    ...overrides,
  };
}

function response(...nodes: unknown[]): unknown {
  return { viewer: { assignedIssues: { nodes } } };
}

test("a node maps to what the column draws, and empty fields are absent", () => {
  expect(ticketFromNode(node())).toEqual({
    id: "WE-142",
    title: "Encoder drops frames",
    url: "https://linear.app/pegasis/issue/WE-142/encoder-drops-frames",
    state: "todo",
    uuid: "3f2c1b4a-0d9e-4a11-9f3b-5c6d7e8f9012",
    team: "WE",
    labels: [],
    children: [],
  });
});

test("every state type Linear models arrives as ours", () => {
  expect(stateOf({ state: { type: "triage" } })).toBe("triage");
  expect(stateOf({ state: { type: "backlog" } })).toBe("backlog");
  expect(stateOf({ state: { type: "unstarted" } })).toBe("todo");
  expect(stateOf({ state: { type: "started" } })).toBe("in_progress");
  expect(stateOf({ state: { type: "completed" } })).toBe("done");
  expect(stateOf({ state: { type: "canceled" } })).toBe("canceled");
  expect(stateOf({ state: { type: "duplicate" } })).toBe("duplicate");
});

// The rule that keeps an issue off the board instead of drawing it as Todo. Every type Linear
// has today is modelled, so what this catches is a type it adds tomorrow.
test("an issue whose state type we do not model is dropped, not defaulted", () => {
  expect(ticketFromNode(node({ state: { type: "somethingNew" } }))).toBeNull();
  expect(ticketFromNode(node({ state: null }))).toBeNull();
});

/** The state's own id and name ride along, for the menu that changes it. A ticket from an
 *  answer that asked only for the type keeps neither rather than inventing them. */
test("the state's id and name come through when the answer carried them", () => {
  const t = ticketFromNode(node({ state: { id: "st-review", name: "In Review", type: "started" } }));

  expect(t?.state).toBe("in_progress");
  expect(t?.stateId).toBe("st-review");
  expect(t?.stateName).toBe("In Review");

  const bare = ticketFromNode(node({ state: { type: "started" } }));
  expect(bare?.stateId).toBeUndefined();
  expect(bare?.stateName).toBeUndefined();
});

test("an issue with no identifier is dropped", () => {
  expect(ticketFromNode(node({ identifier: "" }))).toBeNull();
  expect(ticketFromNode(node({ identifier: null }))).toBeNull();
  expect(ticketFromNode(null)).toBeNull();
});

// Linear sends `0` for "no priority". A badge reading "0" would be a rank nobody set.
test("priority 0 is absence, and only 1 to 4 survive", () => {
  expect(ticketFromNode(node({ priority: 0 }))?.priority).toBeUndefined();
  expect(ticketFromNode(node({ priority: 1 }))?.priority).toBe(1);
  expect(ticketFromNode(node({ priority: 4 }))?.priority).toBe(4);
  expect(ticketFromNode(node({ priority: 5 }))?.priority).toBeUndefined();
  expect(ticketFromNode(node({ priority: null }))?.priority).toBeUndefined();
});

test("a label with no colour of its own falls back to slate", () => {
  const labels = {
    nodes: [{ id: "l1", name: "backend", color: null }, { id: "l2", name: "urgent" }],
  };

  expect(ticketFromNode(node({ labels }))?.labels).toEqual([
    { id: "l1", name: "backend", color: LABEL_FALLBACK_COLOR },
    { id: "l2", name: "urgent", color: LABEL_FALLBACK_COLOR },
  ]);
});

test("a label keeps the colour Linear stores for it, and a nameless one is dropped", () => {
  const labels = {
    nodes: [{ id: "l1", name: "backend", color: "#bec2c8" }, { id: "l2", color: "#ff0000" }],
  };

  expect(ticketFromNode(node({ labels }))?.labels).toEqual([
    { id: "l1", name: "backend", color: "#bec2c8" },
  ]);
});

/** A label the query answered without an id still draws. Nothing can take it off, which is
 *  what the empty id says, and dropping it would lose a label the ticket really carries. */
test("a label with no id keeps its place and carries an empty one", () => {
  const labels = { nodes: [{ name: "backend", color: "#bec2c8" }] };

  expect(ticketFromNode(node({ labels }))?.labels).toEqual([
    { id: "", name: "backend", color: "#bec2c8" },
  ]);
});

test("Linear's own branch name and the description ride along when it sent them", () => {
  const t = ticketFromNode(
    node({ branchName: "pega/we-142-encoder", description: "# Steps\n\nHot-plug a monitor." }),
  );

  expect(t?.branchName).toBe("pega/we-142-encoder");
  expect(t?.description).toBe("# Steps\n\nHot-plug a monitor.");
});

test("a sub-issue in any state is a link, and one we cannot model is dropped", () => {
  const children = {
    nodes: [
      { identifier: "WE-143", title: "Repro", url: "https://linear.app/x/WE-143", state: { type: "completed" } },
      { identifier: "WE-144", title: "Unknown", url: "https://linear.app/x/WE-144", state: { type: "somethingNew" } },
    ],
  };

  expect(ticketFromNode(node({ children }))?.children).toEqual([
    { id: "WE-143", title: "Repro", url: "https://linear.app/x/WE-143", state: "done" },
  ]);
});

test("a parent is a link, and no parent is an absent field", () => {
  const parent = {
    identifier: "WE-100",
    title: "Encoder rework",
    url: "https://linear.app/x/WE-100",
    state: { type: "started" },
  };

  expect(ticketFromNode(node({ parent }))?.parent).toEqual({
    id: "WE-100",
    title: "Encoder rework",
    url: "https://linear.app/x/WE-100",
    state: "in_progress",
  });
  expect(ticketFromNode(node())?.parent).toBeUndefined();
  expect(linkFromNode(null)).toBeNull();
});

test("the response's nodes map in order, and an unmappable one costs only itself", () => {
  const data = response(
    node(),
    node({ identifier: "WE-9", state: { type: "somethingNew" } }),
    node({ identifier: "WE-7" }),
  );

  expect(ticketsFromResponse(data).map((t) => t.id)).toEqual(["WE-142", "WE-7"]);
});

test("a response of some other shape yields no tickets rather than throwing", () => {
  expect(ticketsFromResponse(null)).toEqual([]);
  expect(ticketsFromResponse({ viewer: null })).toEqual([]);
  expect(ticketsFromResponse({ viewer: { assignedIssues: { nodes: "no" } } })).toEqual([]);
});

// The query is a string, so nothing type-checks it. These are the four clauses that decide
// which issues come back at all.
test("the query asks viewer for open assigned issues, newest first", () => {
  expect(OPEN_ISSUES_QUERY).toContain("viewer { assignedIssues(");
  expect(OPEN_ISSUES_QUERY).toContain('state: { type: { in: ["unstarted", "started"] } }');
  expect(OPEN_ISSUES_QUERY).toContain("orderBy: updatedAt");
  expect(OPEN_ISSUES_QUERY).toContain("first: 100");
  expect(OPEN_ISSUES_QUERY).toContain("id identifier title url");
});
