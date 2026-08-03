// The Linear team the operator last opened a ticket in, remembered in localStorage.
//
// It lives here rather than in the dialog for the reason `lastCloneImage` does: a leaf that
// reads storage on mount is not a function of its props, and it would start on a different
// team in Storybook than in the app. TicketModalContainer reads it when the dialog opens and
// writes it back once a ticket has actually been created.
import type { TeamKey } from "~/lib/cloneDraft";

/** localStorage key holding the team key the last ticket was opened in. */
const LAST_TEAM_KEY = "rmng.lastTicketTeam";

/** Remember the team a ticket was created in. Called after the create lands, not on mere
 *  selection: a dropdown someone scrolled past should not become the default. */
export function rememberTicketTeam(team: string): void {
  try {
    localStorage.setItem(LAST_TEAM_KEY, team.toLowerCase());
  } catch {
    // Private mode or storage disabled. The dialog just falls back to the first team.
  }
}

/** The remembered team key, or null when there is none (or storage is unavailable). */
export function lastTicketTeam(): string | null {
  try {
    return localStorage.getItem(LAST_TEAM_KEY);
  } catch {
    return null;
  }
}

/** Which team a freshly opened dialog should start on: the remembered one if a preset still
 *  claims it, else the first. Blank when no preset declares a team at all.
 *
 *  The existence check matters. A preset can lose a label between tickets, and a remembered
 *  key nothing claims would leave the dropdown showing a team the create would then refuse. */
export function preferredTicketTeam(teams: TeamKey[], remembered: string | null): string {
  const wanted = (remembered ?? "").trim().toLowerCase();
  if (wanted !== "" && teams.some((t) => t.key === wanted)) return wanted;
  return teams[0]?.key ?? "";
}
