// What the ticket panel's two menus offer: the team's labels, and its workflow states.
//
// No poll, like `useWorkspaces` and unlike `useTickets`: a team's labels and its workflow change
// when somebody sits down and edits them, which is not something to ask about every minute.
// `teamMeta` caches the answer for the session on top of that, so two panels open on the same
// team share one request and reopening a menu costs nothing.
//
// A lookup that fails answers empty lists and says nothing. The menus then offer nothing, which
// is the truth as far as this browser can tell, and a banner over the ticket would be a second
// error about a thing the operator did not ask for.

import { useEffect, useState } from "react";

import { keysForTeam, teamKeyOf, type IssueRef } from "~/lib/linear/mutations";
import { teamMeta, type TeamMeta } from "~/lib/linear/team";
import type { TicketLabel, TicketWorkflowState } from "~/lib/linear/types";

export interface TeamMetaState {
  /** Every label the ticket's team can carry, sorted by name. */
  labels: TicketLabel[];
  /** The team's workflow, in its own order. */
  states: TicketWorkflowState[];
  /** The lookup is in flight. A menu says so rather than claiming the team offers nothing. */
  loading: boolean;
}

const EMPTY: TeamMeta = { labels: [], states: [] };

/** What `ticket`'s team offers it.
 *
 *  Pass the presets from `GET /api/config`. The team key comes off the ticket the same way
 *  every write derives it, so what the menus offer and the key that would write it are chosen
 *  by one rule. Null asks nothing, which is what a panel with no ticket open wants. */
export function useTeamMeta(
  presets: { labels: string[]; linearKey: string }[],
  ticket: IssueRef | null,
): TeamMetaState {
  const [meta, setMeta] = useState<TeamMeta>(EMPTY);
  const [loading, setLoading] = useState(false);

  const team = ticket ? teamKeyOf(ticket) : "";
  // Keyed on the serialized list rather than the array, because `presets` is a fresh array on
  // every render that reaches this hook.
  const keysKey = JSON.stringify(team === "" ? [] : keysForTeam(presets, team));

  useEffect(() => {
    const keys = JSON.parse(keysKey) as string[];
    if (team === "" || keys.length === 0) {
      setMeta(EMPTY);
      setLoading(false);
      return;
    }
    // Set on unmount, so an answer that lands late cannot write into a gone component.
    let disposed = false;
    setLoading(true);
    void teamMeta(keys, team).then(
      (found) => {
        if (disposed) return;
        setMeta(found);
        setLoading(false);
      },
      () => {
        if (disposed) return;
        setMeta(EMPTY);
        setLoading(false);
      },
    );
    return () => {
      disposed = true;
    };
  }, [team, keysKey]);

  return { labels: meta.labels, states: meta.states, loading };
}
