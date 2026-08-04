// The Linear workspaces the configured keys belong to, as `useWorkspaces` reports them.
//
// Two of them, because one is the case that proves nothing: a menu with a single row looks the
// same whether or not the merge works.

import type { LinearWorkspace } from "~/lib/linear/types";

export function makeWorkspace(over: Partial<LinearWorkspace> = {}): LinearWorkspace {
  return {
    id: "d41ca53e-9014-4d04-b567-5eb5a10a5ac9",
    name: "Personal",
    urlKey: "pegasis",
    ...over,
  };
}

/** Config order, which is the order the menu draws. */
export const linearWorkspaces: LinearWorkspace[] = [
  makeWorkspace(),
  makeWorkspace({ id: "8f1a2b3c-0000-4444-9999-aabbccddeeff", name: "Webapp", urlKey: "webapp-co" }),
];
