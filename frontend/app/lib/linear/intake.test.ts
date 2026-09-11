// The ticket steps both dialogs run, against a fake Linear: which team a reopened dialog
// starts on, and what reaches the clone route once the issue exists.
import { expect, test } from "bun:test";

import { startingTeam, ticketForClone, type LinearPort } from "./intake";
import type { ResolvedIssue } from "./issues";
import type { LinearTicket } from "./types";
import type { PresetRedacted } from "../wire/PresetRedacted";

const presets = [
  { name: "work", labels: ["WE"], linearKey: "lin_we" },
] as unknown as PresetRedacted[];

function issue(over: Partial<ResolvedIssue> = {}): ResolvedIssue {
  return {
    prefix: "we",
    uuid: "uuid-1",
    identifier: "WE-142",
    title: "Fix the login test",
    url: "https://linear.app/x/issue/WE-142",
    branch: "we-142-fix",
    stateType: "unstarted",
    labels: ["bug"],
    ...over,
  };
}

function port(over: Partial<LinearPort> = {}): LinearPort & { started: string[] } {
  const started: string[] = [];
  return {
    started,
    create: async () => {
      throw new Error("not expected");
    },
    find: async (keys, ref) => ({ issue: issue({ identifier: ref.identifier }), key: keys[0] ?? "" }),
    start: async (key) => {
      started.push(key);
      return true;
    },
    people: async () => [],
    ...over,
  };
}

// A preset can lose a label between tickets. Starting on a team no preset claims would leave
// the dropdown showing something the create then refuses.
test("a remembered team nobody claims falls back to the first", () => {
  expect(startingTeam([{ key: "we" }], "ops")).toBe("we");
});

test("the remembered team is compared without case or surrounding space", () => {
  expect(startingTeam([{ key: "we" }, { key: "dev" }], " DEV ")).toBe("dev");
});

test("an existing ticket is looked up and moved to In Progress", async () => {
  const p = port();
  const meta = await ticketForClone(presets, { ticket: "WE-142" }, p);
  expect(meta.ticket).toBe("WE-142");
  expect(meta.branch).toBe("we-142-fix");
  expect(meta.label).toBe("bug");
  expect(p.started).toEqual(["lin_we"]);
});

// The clone is what the operator asked for; a workflow column is not worth failing it over.
test("a move that Linear refuses still starts the clone", async () => {
  const meta = await ticketForClone(
    presets,
    { ticket: "https://linear.app/x/issue/WE-142/fix" },
    port({
      start: async () => {
        throw new Error("no In Progress state");
      },
    }),
  );
  expect(meta.ticket).toBe("WE-142");
});

test("a new ticket is opened with the key of the preset claiming the team", async () => {
  const seen: { key: string; description: string }[] = [];
  const created: LinearTicket = {
    id: "WE-9",
    uuid: "uuid-9",
    team: "WE",
    title: "New one",
    url: "https://linear.app/x/issue/WE-9",
    branchName: "we-9-new-one",
    state: "todo",
    labels: [],
    children: [],
  };
  const meta = await ticketForClone(
    presets,
    {
      team: "we",
      title: "New one",
      description:
        "![x](/api/linear/asset?url=https%3A%2F%2Fuploads.linear.app%2Fa.png)",
    },
    port({
      create: async (key, i) => {
        seen.push({ key, description: i.description });
        return created;
      },
    }),
  );
  // The body's images reach Linear as Linear's own URLs, not as this page's asset proxy.
  expect(seen).toEqual([
    { key: "lin_we", description: "![x](https://uploads.linear.app/a.png)" },
  ]);
  expect(meta.ticket).toBe("WE-9");
  expect(meta.displayName).toBe("New one");
});

test("input holding no ticket id is refused before any request", async () => {
  expect(ticketForClone(presets, { ticket: "nothing here" }, port())).rejects.toThrow(
    /could not find a ticket id/,
  );
});
