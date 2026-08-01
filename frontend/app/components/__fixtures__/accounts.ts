// Per-account usage (display-only) and the pools accounts are drawn from.
//
// One flat list carries BOTH providers' rows, tagged by `provider`, which is the shape of
// `ControlState.claudeAccounts`. Account ids are per-provider, not pool-scoped.

import type { ClaudeUsage } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

/** An imported account with no usage windows yet, which is how one looks before the poller
 *  has reached it. The id is derived from provider and email the way the server derives it,
 *  so overriding either keeps the row internally consistent. */
export function makeUsage(overrides: Partial<ClaudeUsage> = {}): ClaudeUsage {
  const email = overrides.email ?? "alex@example.com";
  const provider = overrides.provider ?? "claude";
  return {
    id: `${provider}|${email}`,
    email,
    provider,
    active: true,
    assignable: true,
    lastUpdated: 1_700_000_000_000,
    ...overrides,
  };
}

export const claudeAccounts: ClaudeUsage[] = [
  makeUsage({
    email: "alex@example.com",
    provider: "claude",
    fiveHour: { pct: 42, resetsAt: null },
    sevenDay: { pct: 61, resetsAt: null },
    fable: { pct: 8, resetsAt: null },
  }),
  makeUsage({
    email: "sam@example.com",
    provider: "claude",
    active: false,
    fiveHour: { pct: 88, resetsAt: null },
    sevenDay: { pct: 73, resetsAt: null },
  }),
  makeUsage({
    email: "alex@openai.com",
    provider: "codex",
    assignable: false,
    // Codex exposes only a weekly (7d) limit now: the 5h window was removed upstream.
    sevenDay: { pct: 40, resetsAt: null },
    resetCredits: 3n,
  }),
];

/** A named pool of accounts. */
export function makeCloneGroup(overrides: Partial<CloneGroup> = {}): CloneGroup {
  return { name: "pooled", accounts: ["alex@example.com"], ...overrides };
}

/** The configured Claude pools: the authoritative list for the pickers. */
export const cloneGroups: CloneGroup[] = [
  makeCloneGroup({ name: "pooled", accounts: ["alex@example.com", "sam@example.com"] }),
  makeCloneGroup({ name: "solo", accounts: ["alex@example.com"] }),
];

/** The configured Codex pools. */
export const codexGroups: CloneGroup[] = [
  makeCloneGroup({ name: "team", accounts: ["alex@openai.com"] }),
];
