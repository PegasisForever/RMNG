// The rules the hook applies around its fetches: which keys get asked, which clone-linked
// identifiers get asked for by name, and what happens when two answers report the same issue.
import { expect, test } from "bun:test";

import { linearKeys, linkedIds, linkedOnly, mergeTickets, upsertTicket } from "./useTickets";
import type { LinearTicket } from "./types";

function preset(linearKey: string): { linearKey: string } {
  return { linearKey };
}

function ticket(id: string, title = id): LinearTicket {
  return {
    id,
    title,
    url: `https://linear.app/x/issue/${id}`,
    state: "todo",
    labels: [],
    children: [],
  };
}

test("every distinct key is asked once, in config order", () => {
  const presets = [preset("lin_a"), preset("lin_b"), preset("lin_a")];

  expect(linearKeys(presets)).toEqual(["lin_a", "lin_b"]);
});

test("a preset with no key is skipped, and surrounding whitespace is not a difference", () => {
  const presets = [preset(""), preset("   "), preset(" lin_a "), preset("lin_a")];

  expect(linearKeys(presets)).toEqual(["lin_a"]);
});

test("no configured key at all asks nothing", () => {
  expect(linearKeys([])).toEqual([]);
});

// Two people assigned to one issue is unusual but not impossible, and the column draws it
// once either way.
test("an issue two keys both report keeps the first key's position", () => {
  const first = [ticket("WE-1", "from the work key"), ticket("WE-2")];
  const second = [ticket("DEV-5"), ticket("WE-1", "from the personal key")];

  const merged = mergeTickets([first, second]);

  expect(merged.map((t) => t.id)).toEqual(["WE-1", "WE-2", "DEV-5"]);
  expect(merged[0]?.title).toBe("from the work key");
});

test("the merge matches identifiers whatever case they came back in", () => {
  const merged = mergeTickets([[ticket("WE-1")], [ticket("we-1")]]);

  expect(merged.map((t) => t.id)).toEqual(["WE-1"]);
});

test("a key that answered nothing costs the merge nothing", () => {
  expect(mergeTickets([[], [ticket("WE-1")], []]).map((t) => t.id)).toEqual(["WE-1"]);
  expect(mergeTickets([])).toEqual([]);
});

// The optimistic half of a write. Without it the panel snaps back to the pre-edit text until
// the refetch lands, and a ticket just created has nothing for its panel to open onto.
test("an edited ticket replaces the entry that shares its identifier, in place", () => {
  const before = [ticket("WE-1", "old"), ticket("WE-2")];

  const after = upsertTicket(before, ticket("we-1", "new"));

  expect(after.map((t) => t.id)).toEqual(["we-1", "WE-2"]);
  expect(after[0]?.title).toBe("new");
});

test("a ticket the list has never seen joins the front, newest first", () => {
  const after = upsertTicket([ticket("WE-1")], ticket("WE-9"));

  expect(after.map((t) => t.id)).toEqual(["WE-9", "WE-1"]);
});

test("the upsert leaves the list it was given alone", () => {
  const before = [ticket("WE-1", "old")];

  upsertTicket(before, ticket("WE-1", "new"));

  expect(before[0]?.title).toBe("old");
});

// --- the clone-linked half ---------------------------------------------------
//
// A clone's own ticket is asked for by identifier, because the open query answers for
// neither a closed ticket nor one assigned to somebody else.

test("clone-linked identifiers are lowercased and asked for once each", () => {
  expect(linkedIds(["WE-142", " dev-88 ", "we-142", ""])).toEqual(["we-142", "dev-88"]);
  expect(linkedIds([])).toEqual([]);
});

test("a linked ticket survives a round that could not re-read it", () => {
  const current = [ticket("WE-1"), ticket("WE-142"), ticket("DEV-88")];

  expect(linkedOnly(current, ["we-142"]).map((t) => t.id)).toEqual(["WE-142"]);
});

test("a linked ticket whose clone is gone is not carried forward", () => {
  expect(linkedOnly([ticket("WE-142")], [])).toEqual([]);
});
