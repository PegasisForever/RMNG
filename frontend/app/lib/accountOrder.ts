// Shared, client-only cosmetic ordering for imported accounts. The account list is an
// unordered pool, so this order carries NO backend meaning — it is a pure display
// preference, persisted in localStorage and NEVER sent to the server.
//
// It lives in a module-level reactive store (not component state) so that the two places
// that render accounts — the Settings "Claude accounts" / "Codex accounts" lists and the
// left sidebar's usage panel — share one order and update together. localStorage's
// `storage` event only fires in OTHER tabs, so a same-tab reorder must notify subscribers
// here explicitly.
//
// The order is bucketed per provider because that is the only granularity the operator can
// drag at: Settings shows one list per provider, and the two pools are independent. The
// storage key and its `Record<string, string[]>` shape are deliberately carried over from
// when the buckets were account GROUPS rather than providers, so an older saved value still
// parses and a user who ordered their accounts before the account model changed keeps that
// order. Its group-named entries match no provider bucket, so they are inert — read past,
// never applied, and left in place rather than migrated (a group's internal order has no
// meaning now that a group is only a membership checklist, not a rendered ordered list).
// The companion `rmng.settings.groupOrder` key is gone for the same reason: nothing renders
// an ordered list of groups any more, so a store for it would have no readers.
import { useSyncExternalStore } from "react";

const ACCT_ORDER_KEY = "rmng.settings.acctOrder";

/** provider ("claude" / "codex") -> ordered account ids. */
export type AcctOrder = Record<string, string[]>;

function loadOrder<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    // localStorage missing (prerender) or unparseable — fall back to no saved order.
    return fallback;
  }
}

function save(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // ignore (private mode / quota / prerender) — ordering is best-effort cosmetics.
  }
}

// Seeded once from storage on first import (client-side under ssr:false).
let acctOrder: AcctOrder = loadOrder<AcctOrder>(ACCT_ORDER_KEY, {});
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) l();
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

export function getAcctOrder(): AcctOrder {
  return acctOrder;
}

export function setAcctOrder(update: (prev: AcctOrder) => AcctOrder): void {
  acctOrder = update(acctOrder);
  save(ACCT_ORDER_KEY, acctOrder);
  emit();
}

/** Stable-sort `items` by a saved list of keys. Items whose key is in `savedOrderKeys` sort
 *  to that position; items not in it (newly imported accounts) keep their original relative
 *  order at the end. Stable, so an SSE re-render preserves the manual order. */
export function ordered<T>(items: T[], savedOrderKeys: string[], keyOf: (item: T) => string): T[] {
  const pos = new Map(savedOrderKeys.map((k, i) => [k, i] as const));
  const END = Number.MAX_SAFE_INTEGER;
  return [...items].sort((a, b) => (pos.get(keyOf(a)) ?? END) - (pos.get(keyOf(b)) ?? END));
}

/** Apply the per-bucket saved order to a list that MIXES buckets, permuting each bucket's
 *  items only among the slots that bucket already occupies.
 *
 *  The sidebar renders one flat list carrying both providers, but a drag can only ever
 *  reorder within a provider (Settings shows the two pools separately), so there is no
 *  saved answer for "does this Claude row belong above that Codex row". Sorting the mixed
 *  list by a per-bucket position would be a non-transitive comparator — items from different
 *  buckets are simply incomparable — so instead each bucket is sorted on its own and dealt
 *  back into the positions it started in, which leaves the provider interleaving untouched. */
export function orderedWithinBuckets<T>(
  items: T[],
  bucketOf: (item: T) => string,
  keyOf: (item: T) => string,
  order: AcctOrder,
): T[] {
  const byBucket = new Map<string, T[]>();
  for (const item of items) {
    const bucket = bucketOf(item);
    const arr = byBucket.get(bucket);
    if (arr) arr.push(item);
    else byBucket.set(bucket, [item]);
  }
  // Each bucket becomes a queue drained in its new order as the original slots are walked.
  const queues = new Map(
    [...byBucket].map(([bucket, group]) => [bucket, ordered(group, order[bucket] ?? [], keyOf)]),
  );
  const taken = new Map<string, number>();
  return items.map((item) => {
    const bucket = bucketOf(item);
    const i = taken.get(bucket) ?? 0;
    taken.set(bucket, i + 1);
    return queues.get(bucket)![i]!;
  });
}

/** Subscribe a component to the shared cosmetic ordering. Re-renders on any reorder, so the
 *  Settings account lists and the sidebar stay in sync within the same tab. */
export function useAccountOrder(): {
  acctOrder: AcctOrder;
  setAcctOrder: typeof setAcctOrder;
} {
  const a = useSyncExternalStore(subscribe, getAcctOrder, getAcctOrder);
  return { acctOrder: a, setAcctOrder };
}
