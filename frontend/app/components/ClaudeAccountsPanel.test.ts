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

const claudeGroups: CloneGroup[] = [
  { name: "pooled", accounts: ["alex@example.com", "sam@example.com"] },
  { name: "personal", accounts: ["alex@example.com"] },
];
const codexGroups: CloneGroup[] = [{ name: "team", accounts: ["alex@openai.com"] }];

test("an account in two pools is listed under both", () => {
  const out = groupAccounts([alex, sam], claudeGroups, []);

  expect(out.map((s) => s.name)).toEqual(["pooled", "personal"]);
  expect(out[0].accounts.map((a) => a.email)).toEqual(["alex@example.com", "sam@example.com"]);
  expect(out[1].accounts.map((a) => a.email)).toEqual(["alex@example.com"]);
});

test("each provider's pools only draw from that provider's accounts", () => {
  const out = groupAccounts([alex, codex], claudeGroups, codexGroups);

  const team = out.find((s) => s.name === "team");
  expect(team?.provider).toBe("codex");
  expect(team?.accounts.map((a) => a.email)).toEqual(["alex@openai.com"]);
  expect(out.find((s) => s.name === "pooled")?.accounts.map((a) => a.email)).toEqual([
    "alex@example.com",
  ]);
});

test("an account no pool claims lands in its provider's leftovers", () => {
  const out = groupAccounts([alex, solo], claudeGroups, []);

  const loose = out.find((s) => s.name === null);
  expect(loose?.accounts.map((a) => a.email)).toEqual(["solo@example.com"]);
});

test("a configured pool with no accounts still renders", () => {
  // This is the state that leaves every clone bound to the pool unassigned, so hiding it
  // would hide the cause.
  const out = groupAccounts([], [{ name: "empty", accounts: [] }], []);

  expect(out).toHaveLength(1);
  expect(out[0]).toEqual({ name: "empty", provider: "claude", accounts: [] });
});

test("the incoming order is kept inside each pool", () => {
  const out = groupAccounts([sam, alex], claudeGroups, []);

  expect(out[0].accounts.map((a) => a.email)).toEqual(["sam@example.com", "alex@example.com"]);
});
