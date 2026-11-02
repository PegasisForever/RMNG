import { describe, expect, test } from "bun:test";

import { type AccountBinding, accountBinding, clonePool, type Provider } from "./accountBinding";
import type { Clone } from "~/lib/types";

/** One side's fields, named once for both providers. */
type Side = { sticky?: string; selection?: string; email?: string };

function clone(provider: Provider, side: Side, group?: string): Clone {
  return {
    id: "h1",
    host: "h1",
    managed: true,
    group,
    [`${provider}Group`]: side.sticky,
    [`${provider}Selection`]: side.selection,
    [`${provider}AccountEmail`]: side.email,
  };
}

// Each shape of a side's fields, with the binding both providers read off it. `title` leaves
// out the provider's name, which the test puts in front.
const SHAPES: [string, Side, string | undefined, AccountBinding][] = [
  [
    "auto in a pool",
    { sticky: "team", selection: "auto", email: "a@x" },
    "team",
    { mode: "pool", pool: "team", email: "a@x", selection: "auto", badge: "team", label: "a@x", title: "a@x (pool team)" },
  ],
  [
    "auto in a pool whose sticky still names the old pool: the clone's pool wins",
    { sticky: "old", selection: "auto", email: "a@x" },
    "team",
    { mode: "pool", pool: "team", email: "a@x", selection: "auto", badge: "team", label: "a@x", title: "a@x (pool team)" },
  ],
  [
    "auto with no pool",
    { selection: "auto", email: "a@x" },
    undefined,
    { mode: "auto", pool: null, email: "a@x", selection: "auto", badge: "auto", label: "a@x", title: "a@x (auto)" },
  ],
  [
    "auto with nothing in scope: no token",
    { selection: "auto" },
    undefined,
    { mode: "auto", pool: null, email: null, selection: "auto", badge: "auto", label: "unassigned", title: "no account assigned" },
  ],
  [
    "pinned inside a pool: the pin, not the pool",
    { selection: "a@x", email: "a@x" },
    "team",
    { mode: "pinned", pool: null, email: "a@x", selection: "a@x", badge: null, label: "a@x", title: "a@x (pinned)" },
  ],
  [
    "pinned with no pool",
    { selection: "a@x", email: "a@x" },
    undefined,
    { mode: "pinned", pool: null, email: "a@x", selection: "a@x", badge: null, label: "a@x", title: "a@x (pinned)" },
  ],
  [
    "legacy: a sticky pool and a group: selection, no shared pool",
    { sticky: "pooled", selection: "group:pooled", email: "a@x" },
    undefined,
    { mode: "pool", pool: "pooled", email: "a@x", selection: "auto", badge: "pooled", label: "a@x", title: "a@x (pool pooled)" },
  ],
  [
    "legacy: a group: selection alone",
    { selection: "group:pooled" },
    undefined,
    { mode: "pool", pool: "pooled", email: null, selection: "auto", badge: "pooled", label: "unassigned", title: "no account assigned" },
  ],
  [
    "legacy: a sticky pool and no selection",
    { sticky: "pooled", email: "a@x" },
    undefined,
    { mode: "pool", pool: "pooled", email: "a@x", selection: "auto", badge: "pooled", label: "a@x", title: "a@x (pool pooled)" },
  ],
  [
    "legacy: no selection is pinned to the account in use",
    { email: "a@x" },
    "team",
    { mode: "pinned", pool: null, email: "a@x", selection: "a@x", badge: null, label: "a@x", title: "a@x (pinned)" },
  ],
  [
    "legacy: no selection and no account",
    {},
    undefined,
    { mode: "pinned", pool: null, email: null, selection: "auto", badge: null, label: "unassigned", title: "no account assigned" },
  ],
];

for (const [provider, name] of [["claude", "Claude"], ["codex", "Codex"]] as const) {
  describe(`accountBinding (${provider})`, () => {
    test.each(SHAPES)("%s", (_, side, group, want) => {
      expect(accountBinding(clone(provider, side, group), provider)).toEqual({
        ...want,
        title: `${name}: ${want.title}`,
      });
    });
  });
}

describe("clonePool", () => {
  test("the shared pool, also when a side is pinned", () => {
    expect(clonePool(clone("claude", { selection: "a@x", email: "a@x" }, "team"))).toBe("team");
  });
  test("a legacy clone: the pool of either side", () => {
    expect(clonePool(clone("codex", { sticky: "pooled", selection: "group:pooled" }))).toBe("pooled");
  });
  test("no pool", () => {
    expect(clonePool(clone("claude", { selection: "auto" }))).toBeNull();
  });
});
