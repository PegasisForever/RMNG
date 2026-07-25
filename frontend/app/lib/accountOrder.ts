// Shared, client-only cosmetic ordering for account groups and the account rows within a
// group. A group is an unordered pool, so this order carries NO backend meaning — it is a
// pure display preference, persisted in localStorage and NEVER sent to the server.
//
// It lives in a module-level reactive store (not component state) so that the two places
// that render accounts — the Settings "Account groups" manager and the left sidebar's usage
// panel — share one order and update together. localStorage's `storage` event only fires in
// OTHER tabs, so a same-tab reorder must notify subscribers here explicitly.
import { useSyncExternalStore } from "react";

const GROUP_ORDER_KEY = "rmng.settings.groupOrder";
const ACCT_ORDER_KEY = "rmng.settings.acctOrder";

export type GroupOrder = string[];
/** groupName -> ordered account ids. */
export type AcctOrder = Record<string, string[]>;

function loadOrder<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    // localStorage missing (SSR/prerender) or unparseable — fall back to no saved order.
    return fallback;
  }
}

function save(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // ignore (private mode / quota / SSR) — ordering is best-effort cosmetics.
  }
}

// Seeded once from storage on first import (client-side under ssr:false).
let groupOrder: GroupOrder = loadOrder<GroupOrder>(GROUP_ORDER_KEY, []);
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

export function getGroupOrder(): GroupOrder {
  return groupOrder;
}
export function getAcctOrder(): AcctOrder {
  return acctOrder;
}

export function setGroupOrder(next: GroupOrder): void {
  groupOrder = next;
  save(GROUP_ORDER_KEY, next);
  emit();
}

export function setAcctOrder(update: (prev: AcctOrder) => AcctOrder): void {
  acctOrder = update(acctOrder);
  save(ACCT_ORDER_KEY, acctOrder);
  emit();
}

/** Stable-sort `items` by a saved list of keys. Items whose key is in `savedOrderKeys` sort
 *  to that position; items not in it (newly appeared groups/accounts) keep their original
 *  relative order at the end. Stable, so an SSE re-render preserves the manual order. */
export function ordered<T>(items: T[], savedOrderKeys: string[], keyOf: (item: T) => string): T[] {
  const pos = new Map(savedOrderKeys.map((k, i) => [k, i] as const));
  const END = Number.MAX_SAFE_INTEGER;
  return [...items].sort((a, b) => (pos.get(keyOf(a)) ?? END) - (pos.get(keyOf(b)) ?? END));
}

/** Subscribe a component to the shared cosmetic ordering. Re-renders on any reorder, so the
 *  Settings manager and the sidebar stay in sync within the same tab. */
export function useAccountOrder(): {
  groupOrder: GroupOrder;
  acctOrder: AcctOrder;
  setGroupOrder: typeof setGroupOrder;
  setAcctOrder: typeof setAcctOrder;
} {
  const g = useSyncExternalStore(subscribe, getGroupOrder, getGroupOrder);
  const a = useSyncExternalStore(subscribe, getAcctOrder, getAcctOrder);
  return { groupOrder: g, acctOrder: a, setGroupOrder, setAcctOrder };
}
