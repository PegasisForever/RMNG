// Pure list surgery for the settings group tree (one pool list, mixed-provider members,
// each member an email string). The dnd component maps item ids to positions and calls
// these; they never touch the DOM, so they are unit-tested here rather than in a browser.
//
// Invariants the UI enforces through these helpers:
// - group order and member order are both significant (saved verbatim in `groups`);
// - no two of the same email in one group (a same-group drop is a no-op returning null);
// - one account in several groups is fine (cross-group drag MOVES by default; the
//   clone-reference button copies instead — both funnel through here).

export interface TreeGroup {
  name: string;
  accounts: string[];
}

/** Reorder the groups themselves. Out-of-range sides are clamped, never throw. */
export function reorderGroups(groups: TreeGroup[], from: number, to: number): TreeGroup[] {
  const next = groups.map((g) => ({ ...g, accounts: [...g.accounts] }));
  const [moved] = next.splice(clampIndex(from, next.length), 1);
  next.splice(clampInsert(to, next.length), 0, moved);
  return next;
}

export interface MemberPos {
  group: number;
  index: number;
}

/**
 * Move the member at `from` to `to` (insert BEFORE `to.index`, which may equal the target
 * group's length to append). Same-group moves reorder; cross-group moves relocate (the
 * source entry is removed). Returns null when the move would duplicate the email inside
 * the target group — the UI then leaves the tree untouched. Out-of-range positions clamp.
 */
export function moveMember(
  groups: TreeGroup[],
  from: MemberPos,
  to: MemberPos,
): TreeGroup[] | null {
  const next = groups.map((g) => ({ ...g, accounts: [...g.accounts] }));
  const src = next[from.group];
  const dst = next[to.group];
  if (!src || !dst) return null;
  const email = src.accounts[clampIndex(from.index, src.accounts.length)];
  if (email === undefined) return null;
  // A same-group move cannot duplicate (the entry is removed first); a cross-group one
  // that lands on an existing member is refused.
  if (from.group !== to.group && dst.accounts.includes(email)) return null;
  src.accounts.splice(clampIndex(from.index, src.accounts.length), 1);
  // Removing an earlier entry in the same group shifts the insert point down one.
  const insertAt =
    from.group === to.group && from.index < to.index ? to.index - 1 : to.index;
  dst.accounts.splice(clampInsert(insertAt, dst.accounts.length), 0, email);
  return next;
}

/**
 * Reference an existing account into a group (the "clone an existing account" button).
 * Null when the group already holds it — the button never offers those, so null is a
 * can't-happen guard, not a branch the UI plans for.
 */
export function addReference(
  groups: TreeGroup[],
  group: number,
  email: string,
): TreeGroup[] | null {
  const next = groups.map((g) => ({ ...g, accounts: [...g.accounts] }));
  const dst = next[group];
  if (!dst || dst.accounts.includes(email)) return null;
  dst.accounts.push(email);
  return next;
}

/**
 * Drop one membership. `orphaned` is true when the email now sits in no group at all —
 * the caller routes that to the account-delete path (the goal's rule: an account in zero
 * groups is removed), otherwise it just saves the new draft.
 */
export function removeMember(
  groups: TreeGroup[],
  group: number,
  email: string,
): { groups: TreeGroup[]; orphaned: boolean } {
  const next = groups.map((g) => ({ ...g, accounts: [...g.accounts] }));
  const src = next[group];
  if (src) src.accounts = src.accounts.filter((e) => e !== email);
  const orphaned = !next.some((g) => g.accounts.includes(email));
  return { groups: next, orphaned };
}

/** Emails in `allEmails` that no group claims — what a groups-touching save deletes. */
export function orphanedAccounts(groups: TreeGroup[], allEmails: string[]): string[] {
  return allEmails.filter((e) => !groups.some((g) => g.accounts.includes(e)));
}

/** Clamp to a readable index (empty array reads undefined, which callers reject). */
function clampIndex(i: number, len: number): number {
  return Math.max(0, Math.min(Math.max(len - 1, 0), i));
}

/** Clamp to an insert position (appending at `len` is legal). */
function clampInsert(i: number, len: number): number {
  return Math.max(0, Math.min(len, i));
}
