// The rules a write applies before it reaches Linear: which key sends it, which state a new
// issue is pinned to, and which fields a patch actually carries. All three decide something
// the mutation name does not say, and all three are wrong in a way no type would catch.
import { expect, test } from "bun:test";

import {
  issueCreateInput,
  issueUpdateInput,
  keysForTeam,
  pickCreateStateId,
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
