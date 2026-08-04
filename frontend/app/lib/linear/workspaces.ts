// Which Linear workspaces the configured keys belong to, and where each one's home page is.
//
// A key is scoped to exactly one workspace, so asking it `organization` is asking "which
// workspace is this key for". Two presets can hold two keys for the same workspace, which is
// why the merge is by organization id and not by key.
//
// Verified against api.linear.app: `organization { id name urlKey }` answers with the key's own
// workspace, and `urlKey` is the slug Linear puts in its own URLs, so `linear.app/<urlKey>` is
// the workspace home with no second lookup.

import { gql } from "~/lib/linear/client";
import type { LinearWorkspace } from "~/lib/linear/types";

/** The workspace behind whichever key posts it. Linear infers it from the key, so the query
 *  takes no argument. */
export const ORGANIZATION_QUERY = "query { organization { id name urlKey } }";

/** Where a workspace's home page lives.
 *
 *  Linear routes every workspace under its slug, so this is the same prefix every issue URL
 *  already carries. A blank slug produces nothing rather than `linear.app/`, which would land
 *  on Linear's marketing site. */
export function workspaceHomeUrl(workspace: LinearWorkspace): string {
  const slug = workspace.urlKey.trim();
  return slug === "" ? "" : `https://linear.app/${slug}`;
}

/** One key's workspace, or null when the answer carries no usable organization.
 *
 *  Null rather than a throw on a malformed answer, for the same reason the ticket mapping drops
 *  a bad node: one workspace missing from a menu is better than a menu that cannot draw. */
export function workspaceFromResponse(data: unknown): LinearWorkspace | null {
  const org = obj(obj(data)?.organization);
  if (!org) return null;
  const id = str(org, "id");
  const urlKey = str(org, "urlKey");
  if (id === "" || urlKey === "") return null;
  // A workspace with no name of its own is drawn by its slug, which is never blank here.
  return { id, name: str(org, "name") || urlKey, urlKey };
}

/** The workspace one key belongs to. Throws whatever `gql` throws, so the caller decides
 *  whether a key that cannot answer is worth saying anything about. */
export async function fetchWorkspace(key: string): Promise<LinearWorkspace | null> {
  return workspaceFromResponse(await gql<unknown>(key, ORGANIZATION_QUERY));
}

/** The distinct workspaces in `found`, in the order their keys were configured.
 *
 *  Config order rather than alphabetical: the first preset is the one the operator reaches for
 *  most, and a menu that reorders itself when a workspace is renamed is a menu whose items move
 *  under the cursor. */
export function mergeWorkspaces(found: (LinearWorkspace | null)[]): LinearWorkspace[] {
  const seen = new Set<string>();
  const merged: LinearWorkspace[] = [];
  for (const workspace of found) {
    if (!workspace || seen.has(workspace.id)) continue;
    seen.add(workspace.id);
    merged.push(workspace);
  }
  return merged;
}

// --- reading an answer nothing type-checked ----------------------------------

type Json = Record<string, unknown>;

function obj(value: unknown): Json | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Json)
    : null;
}

function str(node: Json, key: string): string {
  const value = node[key];
  return typeof value === "string" ? value : "";
}
