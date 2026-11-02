// Every Linear call the browser makes, as one port. Linear is a service of its own, so the
// code that decides what to ask (the ticket module, `./useTickets.ts`; the two dialogs'
// intake, `./intake.ts`) takes the port, and a test passes the in-memory one
// (`./memoryLinear.ts`) where the page passes `linear`, the real one.
import { ensureInProgress, fetchIssueAny, fetchTicketAny } from "~/lib/linear/issues";
import { issueCreate, issueSetLabel, issueSetState, issueSetStateId, issueUpdate } from "~/lib/linear/mutations";
import { fetchTeamPeople } from "~/lib/linear/people";
import { fetchOpenTickets } from "~/lib/linear/queries";
import { teamMeta } from "~/lib/linear/team";
import { fetchWorkspace } from "~/lib/linear/workspaces";

export interface LinearPort {
  // --- the dialogs' intake ---
  create: typeof issueCreate;
  find: typeof fetchIssueAny;
  start: typeof ensureInProgress;
  people: typeof fetchTeamPeople;
  // --- the ticket module ---
  /** One key's open issues. */
  openTickets: typeof fetchOpenTickets;
  /** One issue by identifier, from the first key that sees it. */
  ticket: typeof fetchTicketAny;
  update: typeof issueUpdate;
  /** Into the first state of a kind (`issueSetState`). */
  setState: typeof issueSetState;
  /** Into one named state of the team's workflow (`issueSetStateId`). */
  setStateId: typeof issueSetStateId;
  setLabel: typeof issueSetLabel;
  teamMeta: typeof teamMeta;
  workspace: typeof fetchWorkspace;
}

/** Linear itself. */
export const linear: LinearPort = {
  create: issueCreate,
  find: fetchIssueAny,
  start: ensureInProgress,
  people: fetchTeamPeople,
  openTickets: fetchOpenTickets,
  ticket: fetchTicketAny,
  update: issueUpdate,
  setState: issueSetState,
  setStateId: issueSetStateId,
  setLabel: issueSetLabel,
  teamMeta,
  workspace: fetchWorkspace,
};
