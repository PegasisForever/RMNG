// Account usage, as `ControlState.claudeAccounts` reports it (refreshed server-side every
// ~60s, delivered over SSE): the pools it groups into, and the figures each window's bar draws.
// A bar's "pace" marker is the utilization you'd be at if you spent the quota uniformly across
// the window (elapsed fraction of [resetsAt - windowLength, resetsAt]); fill past the marker
// = burning faster than uniform.
import type { ClaudeSpend, ClaudeUsage } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

export const FIVE_H_MS = 5 * 60 * 60 * 1000;
export const SEVEN_D_MS = 7 * 24 * 60 * 60 * 1000;

export function barColor(pct: number): string {
  if (pct >= 90) return "bg-rose-500";
  if (pct >= 70) return "bg-amber-500";
  return "bg-emerald-500";
}

/** Utilization expected at `now` if the window's quota were spent uniformly. */
export function pacePct(
  resetsAt: string | null,
  windowMs: number,
  now: number,
): number | null {
  if (!resetsAt) return null;
  const reset = Date.parse(resetsAt);
  if (Number.isNaN(reset)) return null;
  const elapsed = windowMs - (reset - now); // ms since window start
  return Math.min(100, Math.max(0, (elapsed / windowMs) * 100));
}

export function spendLine(spend: ClaudeSpend): string {
  const cur = spend.currency === "USD" ? "$" : `${spend.currency} `;
  const used = `${cur}${(spend.usedCents / 100).toFixed(2)}`;
  if (spend.limitCents == null) return used;
  return `${used}/${cur}${(spend.limitCents / 100).toFixed(2)}`;
}

/** One rendered section: a configured pool (both providers' members together), or one
 *  provider's leftovers that belong to no pool. */
export interface AccountSection {
  /** Pool name, or null for the accounts no pool claims. */
  name: string | null;
  /** Leftover sections are per provider; pool sections always read "claude" (vestigial —
   *  a pool holds both providers now, and pool names are unique). */
  provider: "claude" | "codex";
  accounts: ClaudeUsage[];
}

/** Split accounts into their pools, keeping `ordered`'s order inside each one.
 *
 *  An account in several pools appears in each of them: the pools are how a clone's binding
 *  is resolved, so seeing an account once under "Medi" and again under "Personal" is the
 *  point, not a duplicate to collapse.
 *
 *  A configured pool with no accounts still renders. An empty pool is a misconfiguration
 *  worth seeing, and it is exactly the state that leaves clones bound to it unassigned. */
export function groupAccounts(
  ordered: ClaudeUsage[],
  groups: CloneGroup[],
): AccountSection[] {
  const out: AccountSection[] = [];
  // A pool member is an EMAIL, and one address can be imported on BOTH providers, so a
  // single member can claim two rows. The server already reads it that way: `eligible_members`
  // filters the pool's emails against each provider's own store, and `sweep_ungrouped` matches
  // by email across both. Keyed by email alone this map held one row per address — whichever
  // arrived last — so the other provider's row went unclaimed and drew under "Ungrouped"
  // while being a full member of the pool. Seen on CT 204, where hello@talktomedi.com is
  // imported on both sides and the Claude half showed as ungrouped.
  const byEmail = new Map<string, ClaudeUsage[]>();
  for (const a of ordered) {
    const rows = byEmail.get(a.email);
    if (rows) rows.push(a);
    else byEmail.set(a.email, [a]);
  }
  const claimed = new Set<string>();
  for (const group of groups) {
    // One section per pool holding BOTH providers' members: a pool is one binding now,
    // so its Claude and Codex members belong on screen together. Member order is the
    // pool's own order — the same array the settings tree drags around — not the
    // cosmetic account order. A member with no imported row (stale draft email) draws
    // nothing rather than an empty row.
    const accounts: ClaudeUsage[] = [];
    for (const member of group.accounts) {
      for (const row of byEmail.get(member.email) ?? []) {
        // A tagged row shows only its own side; untagged rows show both.
        if (member.provider && (row.provider ?? "claude") !== member.provider)
          continue;
        accounts.push(row);
        claimed.add(row.id);
      }
    }
    out.push({ name: group.name, provider: "claude", accounts });
  }
  for (const provider of ["claude", "codex"] as const) {
    const loose = ordered.filter(
      (a) => (a.provider ?? "claude") === provider && !claimed.has(a.id),
    );
    if (loose.length > 0) out.push({ name: null, provider, accounts: loose });
  }
  return out;
}
