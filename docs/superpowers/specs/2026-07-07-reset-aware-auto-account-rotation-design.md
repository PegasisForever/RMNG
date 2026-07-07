# Reset-aware auto account rotation

**Date:** 2026-07-07
**Status:** Approved design, pending implementation
**Scope:** Claude and Codex auto/group rotation in `crates/control-server/src/{claude,codex}.rs`.

## Problem

The current auto rotator treats the exhausted threshold as a hard gate:

- 5h usage over 80%, or
- 7d usage at least 95%.

When every account in the pool is over one of those limits, `rotate_pool` has no eligible
accounts and leaves clones on their current account. That is too passive for a saturated fleet:
an auto clone can stay on a worse account even when another account is closer to resetting or
less saturated.

## Design

Keep the current sticky eligible-account behavior while at least one account is under the hard
threshold. This preserves prompt-cache locality during normal operation.

When all candidate accounts are exhausted, switch to a reset-aware fallback pool containing all
imported candidate accounts. Rank fallback accounts by:

1. soonest 5h reset time, when available,
2. lower 5h utilization,
3. soonest 7d reset time, when available,
4. lower 7d utilization,
5. fewer assigned clones,
6. random tie-break.

If reset timestamps are missing or unparsable, rank those accounts after accounts with a known
reset time for that same window, then continue with utilization-based ordering. This means missing
reset data degrades to least-saturated behavior instead of blocking rotation.

The fallback remains sticky. A clone keeps its current account when that account is close enough
to the best fallback target, avoiding noisy back-and-forth when every account is similarly bad.
The initial margin will be intentionally conservative:

- keep current account if its known 5h reset is within 15 minutes of the best known 5h reset, or
- when reset times are unavailable, keep current account if its 5h utilization is within 5
  percentage points of the best target.

If the current account is clearly worse than the best target, the rotator pushes the better
account token and updates `Host.{claude,codex}_account_email`, leaving
`Host.{claude,codex}_selection == "auto"` unchanged.

Claude and Codex use the same policy, with provider-specific usage views and host fields.

## Non-goals

- No new user-facing setting.
- No change to the normal under-threshold eligible pool behavior.
- No proactive switching purely to rebalance healthy accounts.
- No change to token refresh or token push mechanics.

## Testing

Add Rust unit coverage for both Claude and Codex:

- normal eligible-account behavior remains sticky and unchanged,
- when all accounts are exhausted, an auto clone moves to the account with the soonest 5h reset,
- fallback uses lower utilization when reset timestamps are missing,
- a clone stays on its current account when it is within the stickiness margin,
- named groups use the same saturated fallback within their configured member list,
- pinned email selections and legacy `selection == None` hosts remain untouched.
