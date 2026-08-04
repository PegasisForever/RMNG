// Which clones are silenced, and what silencing one means for the clones under it.
//
// Muting is a notification filter and nothing else. A muted clone runs, reports, and flags
// itself unread exactly as it did before; the only thing that changes is that the browser stops
// raising a desktop notification when it stops working.
//
// A parent's mute covers its sub clones. That rule is applied here rather than written into the
// stored set, so unmuting a parent restores its children with no second write and no way for the
// two to disagree.

import type { Clone } from "~/lib/types";

/** The stored ids as a set, for the lookups below. */
export function mutedSet(ids: string[]): Set<string> {
  return new Set(ids);
}

/** Whether `clone` is silenced, by its own mute or by an ancestor's.
 *
 *  `byId` resolves the parent chain. Sub clones are one level deep by construction, but this
 *  walks rather than checks one level: a `seen` guard makes a malformed chain terminate instead
 *  of hanging the render that asked. */
export function isMuted(clone: Clone, byId: Map<string, Clone>, muted: Set<string>): boolean {
  const seen = new Set<string>();
  let current: Clone | undefined = clone;
  while (current && !seen.has(current.id)) {
    if (muted.has(current.id)) return true;
    seen.add(current.id);
    current = current.parent ? byId.get(current.parent) : undefined;
  }
  return false;
}

/** Whether `clone` is silenced only because something above it is.
 *
 *  What the menu reads to explain itself: a sub clone of a muted parent has nothing of its own
 *  to unmute, so the item says where the mute actually lives instead of offering a toggle that
 *  would change nothing. */
export function isMutedByParent(
  clone: Clone,
  byId: Map<string, Clone>,
  muted: Set<string>,
): boolean {
  return !muted.has(clone.id) && isMuted(clone, byId, muted);
}

/** The stored set with `id` flipped, sorted so the same set is always the same list.
 *
 *  Sorted here as well as on the server, so an optimistic render and the state that comes back
 *  over SSE are the same array rather than two orderings of one set. */
export function toggleMuted(current: string[], id: string): string[] {
  const next = current.includes(id)
    ? current.filter((x) => x !== id)
    : [...current, id];
  return next.sort();
}

/** Clones by id, for [`isMuted`]'s parent walk. */
export function cloneIndex(hosts: Clone[]): Map<string, Clone> {
  return new Map(hosts.map((h) => [h.id, h]));
}
