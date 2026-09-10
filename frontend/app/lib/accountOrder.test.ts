// The account order is per-provider, but the sidebar renders one list mixing both. These
// cases pin the two properties that make that work — new accounts don't get shuffled to the
// front, and permuting one provider never disturbs where the other provider's rows sit.
import { expect, test } from "bun:test";

import { ordered, orderedAccounts, orderedWithinBuckets } from "./accountOrder";
import type { ClaudeUsage } from "~/lib/types";

// `orderedAccounts` works on real rows: id is `provider|email`, provider may be absent.
const row = (email: string, provider?: "claude" | "codex"): ClaudeUsage => ({
  id: `${provider ?? "claude"}|${email}`,
  email,
  provider,
  active: true,
  assignable: true,
  lastUpdated: 0,
});


type Acct = { id: string; provider: string };

const acct = (id: string, provider: string): Acct => ({ id, provider });
const ids = (rows: Acct[]) => rows.map((a) => a.id);

const bucketOf = (a: Acct) => a.provider;
const keyOf = (a: Acct) => a.id;

test("an account missing from the saved order lands after the ordered ones, not first", () => {
  const rows = [acct("fresh", "claude"), acct("a", "claude"), acct("b", "claude")];

  // "fresh" was imported after the operator last dragged, so it has no saved position.
  expect(ids(ordered(rows, ["b", "a"], keyOf))).toEqual(["b", "a", "fresh"]);
});

test("reordering one provider leaves the other provider's rows in place", () => {
  // Interleaved, as the sidebar receives them: claude, codex, claude, codex.
  const rows = [
    acct("c1", "claude"),
    acct("x1", "codex"),
    acct("c2", "claude"),
    acct("x2", "codex"),
  ];

  const out = orderedWithinBuckets(rows, bucketOf, keyOf, { claude: ["c2", "c1"] });

  // The two claude rows swapped with each other; both codex rows kept their exact slots.
  expect(ids(out)).toEqual(["c2", "x1", "c1", "x2"]);
});

test("a saved order naming an unknown bucket is ignored rather than dropping rows", () => {
  // What a pre-existing localStorage value looks like: keys are old GROUP names, which
  // match no provider bucket.
  const rows = [acct("c1", "claude"), acct("x1", "codex")];

  const out = orderedWithinBuckets(rows, bucketOf, keyOf, { pooled: ["x1", "c1"] });

  expect(ids(out)).toEqual(["c1", "x1"]);
});

test("every input row survives a reorder exactly once", () => {
  const rows = [
    acct("c1", "claude"),
    acct("x1", "codex"),
    acct("c2", "claude"),
    acct("c3", "claude"),
  ];

  // A stale saved order: "gone" was deleted, "c3" was never dragged.
  const out = orderedWithinBuckets(rows, bucketOf, keyOf, { claude: ["gone", "c2", "c1"] });

  expect(ids(out).slice().sort()).toEqual(["c1", "c2", "c3", "x1"]);
  expect(ids(out)).toEqual(["c2", "x1", "c1", "c3"]);
});

test("a row with no provider counts as Claude", () => {
  // `provider` was added to the row after the fact, so an older row has none.
  const rows = orderedAccounts(
    [row("legacy"), row("new", "claude")],
    {},
  );

  expect(rows.claude.map((a) => a.email)).toEqual(["legacy", "new"]);
  expect(rows.codex).toEqual([]);
});

test("each provider's list follows its own saved order", () => {
  const accounts = [
    row("a@x.com", "claude"),
    row("b@x.com", "claude"),
    row("c@x.com", "codex"),
    row("d@x.com", "codex"),
  ];

  const rows = orderedAccounts(accounts, {
    claude: ["claude|b@x.com", "claude|a@x.com"],
    codex: ["codex|d@x.com", "codex|c@x.com"],
  });

  expect(rows.claude.map((a) => a.email)).toEqual(["b@x.com", "a@x.com"]);
  expect(rows.codex.map((a) => a.email)).toEqual(["d@x.com", "c@x.com"]);
});

test("a freshly imported account lands after the ordered ones, not first", () => {
  const accounts = [
    row("fresh@x.com", "claude"),
    row("a@x.com", "claude"),
  ];

  const rows = orderedAccounts(accounts, { claude: ["claude|a@x.com"] });

  expect(rows.claude.map((a) => a.email)).toEqual(["a@x.com", "fresh@x.com"]);
});
