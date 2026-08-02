// The rules a write applies before it reaches Linear: which key sends it, which state a new
// issue is pinned to, and which fields a patch actually carries. All three decide something
// the mutation name does not say, and all three are wrong in a way no type would catch.
import { expect, test } from "bun:test";

import {
  issueCreate,
  issueCreateInput,
  issueSetState,
  issueUpdate,
  issueUpdateInput,
  keysForTeam,
  pickCreateStateId,
  pickStateId,
  teamKeyOf,
} from "./mutations";

function preset(name: string, labels: string[], linearKey: string) {
  return { name, labels, linearKey };
}

// --- which key writes --------------------------------------------------------

test("the preset that claims the team writes it, whatever order it sits in", () => {
  const presets = [preset("personal", ["per"], "lin_p"), preset("work", ["we", "dev"], "lin_w")];

  expect(keysForTeam(presets, "WE")).toEqual(["lin_w", "lin_p"]);
  expect(keysForTeam(presets, "per")).toEqual(["lin_p", "lin_w"]);
});

test("a team nobody claims falls back to every key in config order", () => {
  const presets = [preset("a", ["we"], "lin_a"), preset("b", ["dev"], "lin_b")];

  expect(keysForTeam(presets, "ZZ")).toEqual(["lin_a", "lin_b"]);
  expect(keysForTeam(presets, "")).toEqual(["lin_a", "lin_b"]);
});

test("a preset with no key is skipped, and one key on two presets is listed once", () => {
  const presets = [
    preset("keyless", ["we"], ""),
    preset("work", ["we"], "lin_w"),
    preset("also-work", ["dev"], "lin_w"),
  ];

  expect(keysForTeam(presets, "we")).toEqual(["lin_w"]);
  expect(keysForTeam([], "we")).toEqual([]);
});

// --- where a new issue lands -------------------------------------------------
//
// The load-bearing one. The column draws Todo and In Progress only, so an issue created into
// a team's default Backlog state is created and gone in the same breath.

test("a new issue is pinned to the lowest-position unstarted state", () => {
  const states = [
    { id: "done", type: "completed", position: 3 },
    { id: "later-todo", type: "unstarted", position: 2 },
    { id: "backlog", type: "backlog", position: 0 },
    { id: "todo", type: "unstarted", position: 1 },
    { id: "doing", type: "started", position: 1.5 },
  ];

  expect(pickCreateStateId(states)).toBe("todo");
});

test("a state with no position sorts last rather than first", () => {
  const states = [
    { id: "unranked", type: "unstarted" },
    { id: "todo", type: "unstarted", position: 9 },
  ];

  expect(pickCreateStateId(states)).toBe("todo");
});

test("a team with no unstarted state leaves the choice to Linear", () => {
  expect(pickCreateStateId([{ id: "backlog", type: "backlog", position: 0 }])).toBeNull();
  expect(pickCreateStateId([])).toBeNull();
  expect(pickCreateStateId(null)).toBeNull();
  expect(pickCreateStateId("no")).toBeNull();
});

test("the create input carries the team, the assignee and the pinned state", () => {
  expect(
    issueCreateInput({
      teamId: "team-uuid",
      title: "Encoder drops frames",
      description: "# Steps",
      viewerId: "user-uuid",
      stateId: "todo-uuid",
      priority: 2,
    }),
  ).toEqual({
    teamId: "team-uuid",
    title: "Encoder drops frames",
    description: "# Steps",
    assigneeId: "user-uuid",
    stateId: "todo-uuid",
    priority: 2,
  });
});

// An app or OAuth actor has no personal user. Leaving the field off keeps creation working.
// Sending null would too, but the ticket then belongs to nobody either way.
test("no viewer, no state and no priority leave their fields off entirely", () => {
  const input = issueCreateInput({
    teamId: "team-uuid",
    title: "t",
    description: "",
    viewerId: null,
    stateId: null,
  });

  expect(input).toEqual({ teamId: "team-uuid", title: "t", description: "" });
});

test("priority 0 is no priority, and only whole ranks are sent", () => {
  const base = { teamId: "t", title: "t", description: "" };

  expect(issueCreateInput({ ...base, priority: 0 }).priority).toBeUndefined();
  expect(issueCreateInput({ ...base, priority: 1.5 }).priority).toBeUndefined();
  expect(issueCreateInput({ ...base, priority: 4 }).priority).toBe(4);
});

// The dialog reaches `issueCreate` with `?? ""` when no preset holds a key for the chosen
// team, so a blank key is a real caller and not a type error. Both writes now say the same
// sentence about it. No request is made, which is what makes this test safe to run.
test("opening an issue with no key says so rather than asking Linear", async () => {
  const issue = { team: "WE", title: "Repro", description: "" };

  await expect(issueCreate("", issue)).rejects.toThrow(/no Linear API key configured/);
  await expect(issueCreate("   ", issue)).rejects.toThrow(/no Linear API key configured/);
  await expect(issueUpdate([], { id: "WE-1" }, { title: "x" })).rejects.toThrow(
    /no Linear API key configured/,
  );
});

// --- what an edit carries ----------------------------------------------------

test("an omitted field is left alone and an empty description is a real value", () => {
  expect(issueUpdateInput({ title: "New title" })).toEqual({ title: "New title" });
  expect(issueUpdateInput({ description: "" })).toEqual({ description: "" });
  expect(issueUpdateInput({ title: "t", description: "d" })).toEqual({
    title: "t",
    description: "d",
  });
});

// The caller's signal that there is nothing to send. `issueUpdate` returns without a request.
test("a patch that asks for nothing builds nothing", () => {
  expect(issueUpdateInput({})).toEqual({});
});

// --- moving a ticket to another state ----------------------------------------
//
// The column's Cancel and Move-to-backlog items. Both name a state *type*, so a workspace
// that calls its backlog "Icebox" is moved into that one.

test("a state is picked by type, lowest position first", () => {
  const nodes = [
    { id: "s-done", type: "completed", position: 0 },
    { id: "s-cancel-late", type: "canceled", position: 3 },
    { id: "s-cancel", type: "canceled", position: 1 },
    { id: "s-icebox", type: "backlog", position: 2 },
  ];

  expect(pickStateId(nodes, "canceled")).toBe("s-cancel");
  expect(pickStateId(nodes, "backlog")).toBe("s-icebox");
  expect(pickStateId(nodes, "started")).toBeNull();
});

// The create path is that same rule asking for Todo, so the two cannot drift apart.
test("a new issue still lands in the lowest Todo", () => {
  const nodes = [
    { id: "s-triage", type: "unstarted", position: 5 },
    { id: "s-todo", type: "unstarted", position: 1 },
  ];

  expect(pickCreateStateId(nodes)).toBe(pickStateId(nodes, "unstarted"));
  expect(pickCreateStateId(nodes)).toBe("s-todo");
});

test("the workflow is looked up by the ticket's team, or by its identifier's own prefix", () => {
  expect(teamKeyOf({ id: "WE-142", team: "we" })).toBe("WE");
  expect(teamKeyOf({ id: "dev-88" })).toBe("DEV");
  expect(teamKeyOf({ id: "WE-142", team: "  " })).toBe("WE");
});

// No request is made, which is what makes this safe to run.
test("moving a ticket with no key says so rather than asking Linear", async () => {
  await expect(issueSetState([], { id: "WE-1" }, "canceled")).rejects.toThrow(
    /no Linear API key configured/,
  );
});
