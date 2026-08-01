// The collapsed usage row shows one number for the whole fleet, so that number has to be
// the one worth reacting to: the highest gating window across every account.
import { expect, test } from "bun:test";

import { peakUsage } from "./MobileUsageSection";
import type { ClaudeUsage } from "~/lib/types";

function account(email: string, fiveHour?: number, sevenDay?: number): ClaudeUsage {
  return {
    id: `${email}|org`,
    email,
    active: false,
    lastUpdated: 0,
    ...(fiveHour === undefined ? {} : { fiveHour: { pct: fiveHour, resetsAt: null } }),
    ...(sevenDay === undefined ? {} : { sevenDay: { pct: sevenDay, resetsAt: null } }),
  };
}

test("no accounts means no peak", () => {
  expect(peakUsage([])).toBeNull();
});

test("an account with no window data yet contributes nothing", () => {
  expect(peakUsage([account("alex@example.com")])).toBeNull();
});

test("the highest window across all accounts wins, and it names the account", () => {
  const peak = peakUsage([account("alex@example.com", 42, 61), account("sam@example.com", 88, 73)]);

  expect(peak).toEqual({ pct: 88, email: "sam@example.com", window: "5h" });
});

test("the 7d window can be the peak", () => {
  const peak = peakUsage([account("alex@example.com", 12, 96)]);

  expect(peak).toEqual({ pct: 96, email: "alex@example.com", window: "7d" });
});

test("the Fable window never counts, since it gates nothing", () => {
  const fableHeavy: ClaudeUsage = {
    ...account("alex@example.com", 10, 20),
    fable: { pct: 99, resetsAt: null },
  };

  expect(peakUsage([fableHeavy])).toEqual({ pct: 20, email: "alex@example.com", window: "7d" });
});
