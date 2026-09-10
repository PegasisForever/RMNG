import { expect, test } from "bun:test";

import { groupAccounts } from "./ClaudeAccountsPanel";
import type { ClaudeUsage } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

const account = (email: string, provider: "claude" | "codex" = "claude"): ClaudeUsage => ({
  id: `${provider}|${email}`,
  email,
  provider,
  active: true,
  assignable: true,
  lastUpdated: 0,
});

const alex = account("alex@example.com");
const sam = account("sam@example.com");
const solo = account("solo@example.com");
const codex = account("alex@openai.com", "codex");

const groups: CloneGroup[] = [
  { name: "pooled", accounts: ["alex@example.com", "sam@example.com"] },
  { name: "personal", accounts: ["alex@example.com"] },
  { name: "team", accounts: ["alex@openai.com", "sam@example.com"] },
];

test("an account in two pools is listed under both", () => {
  const out = groupAccounts([alex, sam], groups);

  expect(out.map((s) => s.name)).toEqual(["pooled", "personal", "team"]);
  expect(out[0].accounts.map((a) => a.email)).toEqual(["alex@example.com", "sam@example.com"]);
  expect(out[1].accounts.map((a) => a.email)).toEqual(["alex@example.com"]);
});

test("a pool section holds both providers' members together", () => {
  const out = groupAccounts([alex, codex, sam], groups);

  const team = out.find((s) => s.name === "team");
  expect(team?.accounts.map((a) => a.email)).toEqual(["alex@openai.com", "sam@example.com"]);
  expect(out.find((s) => s.name === "pooled")?.accounts.map((a) => a.email)).toEqual([
    "alex@example.com",
    "sam@example.com",
  ]);
});

test("an account no pool claims lands in its provider's leftovers", () => {
  const out = groupAccounts([alex, solo], groups);

  const loose = out.filter((s) => s.name === null);
  expect(loose).toHaveLength(1);
  expect(loose[0].accounts.map((a) => a.email)).toEqual(["solo@example.com"]);
});

test("a configured pool with no accounts still renders", () => {
  // This is the state that leaves every clone bound to the pool unassigned, so hiding it
  // would hide the cause.
  const out = groupAccounts([], [{ name: "empty", accounts: [] }]);

  expect(out).toHaveLength(1);
  expect(out[0]).toEqual({ name: "empty", provider: "claude", accounts: [] });
});

test("member order follows the pool, not the incoming rows", () => {
  // The pool lists sam before alex; the rows arrive reversed. The settings tree owns the
  // order, so the usage column matches it rather than the cosmetic account order.
  const out = groupAccounts([sam, alex], [
    { name: "pooled", accounts: ["alex@example.com", "sam@example.com"] },
  ]);

  expect(out[0].accounts.map((a) => a.email)).toEqual(["alex@example.com", "sam@example.com"]);
});

test("a pool member with no imported row draws nothing", () => {
  const out = groupAccounts([alex], [{ name: "pooled", accounts: ["alex@example.com", "stale@x.com"] }]);

  expect(out[0].accounts.map((a) => a.email)).toEqual(["alex@example.com"]);
});
