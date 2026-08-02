// The rules the clone dialog applies between "an operator typed WE-142" and "a clone exists".
// Each one decides something the function name does not say, and each one is wrong in a way no
// type would catch: which halves the lookup filters by, which state an issue is moved into,
// which issues are left alone, and which fields reach the clone route.
import { expect, test } from "bun:test";

import {
  cloneLinearMeta,
  ensureInProgress,
  fetchIssueAny,
  issueFromNode,
  issueRefOf,
  needsInProgress,
  pickInProgressStateId,
  resolvedFromTicket,
  stateTypeOf,
  type ResolvedIssue,
} from "./issues";
import type { LinearTicket } from "./types";

// --- splitting the identifier the query filters by ---------------------------

test("a link and a bare id both split into the team key and number Linear filters by", () => {
  expect(issueRefOf("https://linear.app/acme/issue/WE-142/encoder-drops-frames")).toEqual({
    teamKey: "WE",
    number: 142,
    identifier: "WE-142",
    prefix: "we",
  });
  expect(issueRefOf("  we-142 ")).toEqual({
    teamKey: "WE",
    number: 142,
    identifier: "WE-142",
    prefix: "we",
  });
});

test("input with no ticket id in it resolves to nothing rather than a request", () => {
  expect(issueRefOf("")).toBeNull();
  expect(issueRefOf("nope")).toBeNull();
  expect(issueRefOf("W-1")).toBeNull();
});

// --- reading one issue out of the answer -------------------------------------

const NODE = {
  id: "8f2c-uuid",
  identifier: "WE-142",
  title: "Encoder drops frames",
  url: "https://linear.app/acme/issue/WE-142/encoder-drops-frames",
  branchName: "pegasis/we-142-encoder-drops-frames",
  state: { id: "s1", name: "Todo", type: "unstarted" },
  labels: { nodes: [{ name: "backend" }, { name: "urgent" }] },
};

test("an issue node maps to what a clone stores, plus the state the move reads", () => {
  expect(issueFromNode("we", NODE)).toEqual({
    prefix: "we",
    uuid: "8f2c-uuid",
    identifier: "WE-142",
    title: "Encoder drops frames",
    url: "https://linear.app/acme/issue/WE-142/encoder-drops-frames",
    branch: "pegasis/we-142-encoder-drops-frames",
    stateType: "unstarted",
    labels: ["backend", "urgent"],
  });
});

// The reason this shape is not `LinearTicket`: that one drops an issue whose state type it
// cannot model, and a `triage` ticket is a real ticket somebody wants a clone for.
test("a state type nothing models is carried verbatim, not dropped", () => {
  expect(issueFromNode("we", { ...NODE, state: { type: "triage" } })?.stateType).toBe("triage");
});

test("an issue with no branch name and no labels still maps", () => {
  const sparse = { identifier: "WE-7", title: "Quick fix" };

  expect(issueFromNode("we", sparse)).toEqual({
    prefix: "we",
    uuid: "",
    identifier: "WE-7",
    title: "Quick fix",
    url: "",
    branch: "",
    stateType: "",
    labels: [],
  });
});

test("a node with no identifier is nothing, which is what makes a miss readable", () => {
  expect(issueFromNode("we", { title: "orphan" })).toBeNull();
  expect(issueFromNode("we", null)).toBeNull();
  expect(issueFromNode("we", undefined)).toBeNull();
});

test("asking with no key at all says so rather than reporting a missing ticket", async () => {
  const ref = { teamKey: "WE", number: 1, identifier: "WE-1", prefix: "we" };

  await expect(fetchIssueAny([], ref)).rejects.toThrow(/no preset has a Linear API key/);
  await expect(fetchIssueAny(["", "   "], ref)).rejects.toThrow(/no preset has a Linear API key/);
});

// --- which state an issue is moved into --------------------------------------
//
// The load-bearing one. A team with "In Progress" and "In Review" has two started states, and
// landing a fresh clone in review is wrong in a way nobody notices until standup.

test("a state named In Progress wins over any other started state", () => {
  const states = [
    { id: "review", name: "In Review", type: "started" },
    { id: "doing", name: "In Progress", type: "started" },
    { id: "todo", name: "Todo", type: "unstarted" },
  ];

  expect(pickInProgressStateId(states)).toBe("doing");
});

test("a team with no such name falls back to its first started state", () => {
  const states = [
    { id: "todo", name: "Todo", type: "unstarted" },
    { id: "building", name: "Building", type: "started" },
    { id: "review", name: "In Review", type: "started" },
  ];

  expect(pickInProgressStateId(states)).toBe("building");
});

test("a team with no started state at all answers nothing", () => {
  expect(pickInProgressStateId([{ id: "todo", name: "Todo", type: "unstarted" }])).toBeNull();
  expect(pickInProgressStateId([{ name: "In Progress", type: "started" }])).toBeNull();
  expect(pickInProgressStateId([])).toBeNull();
  expect(pickInProgressStateId(null)).toBeNull();
});

// --- and which issues are left where they are --------------------------------

test("an already-started issue is never dragged backwards", () => {
  expect(needsInProgress("started")).toBe(false);
  expect(needsInProgress("unstarted")).toBe(true);
  expect(needsInProgress("backlog")).toBe(true);
  expect(needsInProgress("triage")).toBe(true);
  // Cloning a cancelled or completed ticket is picking it back up, so both move forward.
  expect(needsInProgress("canceled")).toBe(true);
  expect(needsInProgress("completed")).toBe(true);
});

// No key is needed, and no request is made: the guard answers before the network would.
test("moving a started issue makes no request and reports that it wrote nothing", async () => {
  const issue: ResolvedIssue = {
    prefix: "we",
    uuid: "8f2c-uuid",
    identifier: "WE-142",
    title: "t",
    url: "",
    branch: "",
    stateType: "started",
    labels: [],
  };

  expect(await ensureInProgress("", issue)).toBe(false);
});

// --- what reaches the clone route --------------------------------------------

test("the clone route is sent Linear's own answer, first label only", () => {
  expect(cloneLinearMeta(issueFromNode("we", NODE)!)).toEqual({
    workspace: "we",
    ticket: "WE-142",
    ticketUrl: "https://linear.app/acme/issue/WE-142/encoder-drops-frames",
    branch: "pegasis/we-142-encoder-drops-frames",
    title: "Encoder drops frames",
    label: "backend",
  });
});

test("an issue with no labels sends no label field at all", () => {
  const meta = cloneLinearMeta(issueFromNode("we", { ...NODE, labels: { nodes: [] } })!);

  expect("label" in meta).toBe(false);
});

// --- a ticket that was just opened, on its way to the same two steps ----------

test("Linear's state types and ours map both ways", () => {
  expect(stateTypeOf("todo")).toBe("unstarted");
  expect(stateTypeOf("in_progress")).toBe("started");
  expect(stateTypeOf("backlog")).toBe("backlog");
  expect(stateTypeOf("done")).toBe("completed");
  expect(stateTypeOf("canceled")).toBe("canceled");
});

test("a newly opened ticket resolves the same way a looked-up one does", () => {
  const created: LinearTicket = {
    id: "WE-143",
    uuid: "b41d-uuid",
    title: "New thing",
    url: "https://linear.app/acme/issue/WE-143/new-thing",
    state: "todo",
    team: "WE",
    branchName: "pegasis/we-143-new-thing",
    labels: [{ name: "backend", color: "#94a3b8" }],
    children: [],
  };

  const issue = resolvedFromTicket(created);

  expect(issue.prefix).toBe("we");
  expect(issue.identifier).toBe("WE-143");
  expect(issue.uuid).toBe("b41d-uuid");
  expect(issue.branch).toBe("pegasis/we-143-new-thing");
  expect(issue.labels).toEqual(["backend"]);
  // Todo, so the move still has work to do, which is what the server did after creating one.
  expect(needsInProgress(issue.stateType)).toBe(true);
});

test("a created ticket that carried no team takes its prefix from its own identifier", () => {
  const created: LinearTicket = {
    id: "PER-19",
    title: "Scratch",
    url: "",
    state: "canceled",
    labels: [],
    children: [],
  };

  const issue = resolvedFromTicket(created);

  expect(issue.prefix).toBe("per");
  expect(issue.uuid).toBe("");
  expect(issue.branch).toBe("");
});
