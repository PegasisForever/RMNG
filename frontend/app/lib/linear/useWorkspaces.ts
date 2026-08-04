// The workspaces behind the configured Linear keys, fetched once per key.
//
// No poll, unlike `useTickets`. A workspace's name and slug change about never, and the only
// thing that can change the LIST is the operator editing a key in Settings, which restarts the
// effect on its own. So this asks at mount and stays quiet.
//
// A key that cannot answer contributes nothing and says nothing. The menu this feeds is a
// convenience link, so a workspace missing from it is worth less than a banner would cost.

import { useEffect, useState } from "react";

import type { LinearWorkspace } from "~/lib/linear/types";
import { linearKeys } from "~/lib/linear/useTickets";
import { fetchWorkspace, mergeWorkspaces } from "~/lib/linear/workspaces";

/** Every distinct workspace the presets' keys belong to, in config order.
 *
 *  Pass the presets from `GET /api/config`; the keys are read off them the same way
 *  `useTickets` reads them, so the two always agree on which keys exist. */
export function useWorkspaces(presets: { linearKey: string }[]): LinearWorkspace[] {
  const [workspaces, setWorkspaces] = useState<LinearWorkspace[]>([]);

  // Keyed on the serialized list rather than the array, because `presets` is a fresh array on
  // every render that reaches this hook.
  const keysKey = JSON.stringify(linearKeys(presets));

  useEffect(() => {
    const keys = JSON.parse(keysKey) as string[];
    if (keys.length === 0) {
      setWorkspaces([]);
      return;
    }
    // Set on unmount, so an answer that lands late cannot write into a gone component.
    let disposed = false;
    void Promise.all(keys.map((key) => fetchWorkspace(key).catch(() => null))).then((found) => {
      if (!disposed) setWorkspaces(mergeWorkspaces(found));
    });
    return () => {
      disposed = true;
    };
  }, [keysKey]);

  return workspaces;
}
