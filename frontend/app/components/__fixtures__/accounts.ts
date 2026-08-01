// Per-account usage (display-only) and the pools accounts are drawn from.
//
// One flat list carries BOTH providers' rows, tagged by `provider`, which is the shape of
// `ControlState.claudeAccounts`. Account ids are per-provider, not pool-scoped.
//
// Everything here is a builder. There are no shared module-level lists: the pickers and the
// usage panel are handed these arrays as props and several of the components downstream copy
// them into state, so one array behind every story is how an edit in one story shows up in
// the next. Call the builder per story and the graphs stay separate.

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

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

/** Both providers' rows, freshly built.
 *
 *  `now` anchors the reset instants, and they are relative to it rather than fixed because
 *  `ClaudeAccountsPanel` reads the live clock for both things a `resetsAt` drives: the
 *  hover tooltip's countdown and the vertical pace marker on each bar. A pinned instant would
 *  be in the past by the time anyone opened the story, and every bar would then draw at 100%
 *  pace with a "Reset …" tooltip, which is one state out of several and the least useful one.
 *  Pass an explicit `now` to make a story's bars reproducible.
 *
 *  One window is deliberately left with `resetsAt: null`, because that is what every row
 *  looks like before the poller has filled it in: no marker and no tooltip. */
export function makeClaudeAccounts(now: number = Date.now()): ClaudeUsage[] {
  const at = (ms: number) => new Date(now + ms).toISOString();
  return [
    makeUsage({
      email: "alex@example.com",
      provider: "claude",
      fiveHour: { pct: 42, resetsAt: at(2 * HOUR + 15 * MINUTE) },
      sevenDay: { pct: 61, resetsAt: at(76 * HOUR) },
      fable: { pct: 8, resetsAt: at(76 * HOUR) },
    }),
    makeUsage({
      email: "sam@example.com",
      provider: "claude",
      active: false,
      // Nearly through its 5h window and spending faster than uniform, so the fill sits just
      // past the pace marker.
      fiveHour: { pct: 88, resetsAt: at(40 * MINUTE) },
      sevenDay: { pct: 73, resetsAt: null },
    }),
    makeUsage({
      email: "alex@openai.com",
      provider: "codex",
      assignable: false,
      // Codex exposes only a weekly (7d) limit now: the 5h window was removed upstream.
      sevenDay: { pct: 40, resetsAt: at(120 * HOUR) },
      resetCredits: 3n,
    }),
  ];
}

/** A named pool of accounts. */
export function makeCloneGroup(overrides: Partial<CloneGroup> = {}): CloneGroup {
  return { name: "pooled", accounts: ["alex@example.com"], ...overrides };
}

/** The configured Claude pools, freshly built, down to each pool's member array. */
export function makeCloneGroups(): CloneGroup[] {
  return [
    makeCloneGroup({ name: "pooled", accounts: ["alex@example.com", "sam@example.com"] }),
    makeCloneGroup({ name: "solo", accounts: ["alex@example.com"] }),
  ];
}

/** The configured Codex pools, freshly built. */
export function makeCodexGroups(): CloneGroup[] {
  return [makeCloneGroup({ name: "team", accounts: ["alex@openai.com"] })];
}
