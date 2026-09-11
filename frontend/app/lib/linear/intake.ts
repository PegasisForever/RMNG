// The ticket a dialog is about: the team it starts on, who holds it, and finding or opening
// the issue itself. Both dialogs come here — the board's ticket dialog to open one, the clone
// dialog to open or find the one a clone is named after — so a default set in one is the
// default in the other. Linear is a port, so a test drives the whole flow with a fake.
import { useEffect, useState } from "react";

import { toLinearMarkdown } from "~/lib/linear/assets";
import {
  cloneLinearMeta,
  ensureInProgress,
  fetchIssueAny,
  issueRefOf,
  resolvedFromTicket,
  type ResolvedIssue,
} from "~/lib/linear/issues";
import {
  issueCreate,
  keysForTeam,
  type NewIssue,
} from "~/lib/linear/mutations";
import {
  defaultAssignee,
  fetchTeamPeople,
  type TicketPerson,
} from "~/lib/linear/people";
import type { LinearTicket } from "~/lib/linear/types";
import type { LinearMeta } from "~/lib/wire/LinearMeta";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

/** Every Linear call the intake makes. A test passes its own. */
export interface LinearPort {
  create: typeof issueCreate;
  find: typeof fetchIssueAny;
  start: typeof ensureInProgress;
  people: typeof fetchTeamPeople;
}

export const linear: LinearPort = {
  create: issueCreate,
  find: fetchIssueAny,
  start: ensureInProgress,
  people: fetchTeamPeople,
};

const LAST_TEAM_KEY = "rmng.lastTicketTeam";

/** The team the last ticket was opened in, or null. Storage can be unavailable. */
export function lastTeam(): string | null {
  try {
    return localStorage.getItem(LAST_TEAM_KEY);
  } catch {
    return null;
  }
}

/** Which team a freshly opened dialog starts on: the remembered one if a preset still claims
 *  it, else the first. A preset can lose a label between tickets, and a team nothing claims
 *  would leave the create to be refused. */
export function startingTeam(
  teams: { key: string }[],
  remembered = lastTeam(),
): string {
  const wanted = (remembered ?? "").trim().toLowerCase();
  if (wanted !== "" && teams.some((t) => t.key === wanted)) return wanted;
  return teams[0]?.key ?? "";
}

/** The key that opens tickets for `team`. Blank when no preset holds one, which `issueCreate`
 *  refuses rather than sends. */
export function keyFor(presets: PresetRedacted[], team: string): string {
  return keysForTeam(presets, team.trim())[0] ?? "";
}

/** Open a new issue, body converted to Linear's own markdown, and remember its team. Only a
 *  create that landed: a team someone scrolled past should not become the next default. */
export async function openTicket(
  presets: PresetRedacted[],
  issue: NewIssue,
  port: LinearPort = linear,
): Promise<LinearTicket> {
  const created = await port.create(keyFor(presets, issue.team), {
    ...issue,
    description: toLinearMarkdown(issue.description),
  });
  try {
    localStorage.setItem(LAST_TEAM_KEY, issue.team.toLowerCase());
  } catch {
    // Private mode: the next dialog just starts on the first team.
  }
  return created;
}

/** The issue a clone is for: the one `want.ticket` names, or a new one. Moved to In Progress
 *  on the way, best effort — a workflow column is not worth failing a clone over. */
export async function ticketForClone(
  presets: PresetRedacted[],
  want: { ticket: string } | NewIssue,
  port: LinearPort = linear,
): Promise<LinearMeta> {
  let issue: ResolvedIssue;
  let key: string;
  if ("ticket" in want) {
    const ref = issueRefOf(want.ticket);
    if (!ref)
      throw new Error(
        `could not find a ticket id (like WE-142) in "${want.ticket}"`,
      );
    ({ issue, key } = await port.find(keysForTeam(presets, ref.prefix), ref));
  } else {
    key = keyFor(presets, want.team);
    issue = resolvedFromTicket(await openTicket(presets, want, port));
  }
  try {
    await port.start(key, issue);
  } catch (e) {
    console.warn(`could not move ${issue.identifier} to In Progress:`, e);
  }
  return cloneLinearMeta(issue);
}

/** The team's assignable people and who the dialog starts on, which is you. A lookup that
 *  fails leaves the list empty rather than blocking the dialog: the create then falls back to
 *  the key's own owner. A late answer for a team no longer chosen is dropped. */
export function useAssignee(key: string, team: string, port: LinearPort = linear) {
  const [people, setPeople] = useState<TicketPerson[]>([]);
  const [loading, setLoading] = useState(false);
  const [assigneeId, setAssigneeId] = useState("");
  useEffect(() => {
    if (key === "" || team === "") {
      setPeople([]);
      setAssigneeId("");
      return;
    }
    let live = true;
    setLoading(true);
    port
      .people(key, team)
      .then(
        (found) => {
          if (!live) return;
          setPeople(found);
          setAssigneeId(defaultAssignee(found));
        },
        () => {
          if (!live) return;
          setPeople([]);
          setAssigneeId("");
        },
      )
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
  }, [key, team, port]);
  return { people, loading, assigneeId, setAssigneeId };
}
