// The Fork dialog's model: what the operator can change about a copy of one clone, and the
// request it sends (`POST /api/fork`, `~/components/ForkModalContainer.tsx`).
//
// The rules:
// - A clone with a ticket: the fork is for the same ticket, always. The request sends the
//   source's ticket, so the server names the fork from the ticket (`pega-we-142b`) and adds
//   the letter to its title. With no ticket sent, the server would keep the ticket all the
//   same, but name the fork from the source's own id (`pega-we-142aa`).
// - A clone with no ticket: the operator gives the fork a name, which names it.
// - A sub clone, by default: it shows under the source. A sub clone is never a parent, so the
//   fork of a sub clone is a sub clone of the same parent.
// - A message, when typed, starts the agent on it (the server sends it in place of the
//   ticket link on a fork).
import type { Clone } from "~/lib/types";
import type { CloneRequest } from "~/lib/wire/CloneRequest";

export interface ForkDraft {
  /** The fork's name. Only for a source with no ticket. */
  name: string;
  /** Sent to the agent as its first turn. Empty: the agent does not start. */
  message: string;
  /** Show the fork under its parent (`forkParent`). */
  sub: boolean;
  /** The pool both sides draw from: the source's, until changed. */
  group: string;
  /** "" follows the group. */
  claudeAccount: string;
  codexAccount: string;
  headless: boolean;
  runStartupScript: boolean;
  rebuild: boolean;
}

/** The form as the dialog opens it for `source`. */
export function emptyForkDraft(source: Clone): ForkDraft {
  return {
    name: "",
    message: "",
    sub: true,
    group: sourceGroup(source),
    claudeAccount: "",
    codexAccount: "",
    headless: source.headless ?? false,
    runStartupScript: true,
    rebuild: false,
  };
}

/** The source's pool as the group picker shows it: "none" is any pool. */
const sourceGroup = (source: Clone) => source.group ?? "none";

/** Whether the fork takes the source's ticket. */
export const hasTicket = (source: Clone) => !!source.linearTicket;

/** The clone a sub clone fork goes under: the source, or the source's own parent. */
export const forkParent = (source: Clone) => source.parent ?? source.id;

/** The Fork button may fire. */
export const forkValid = (source: Clone, d: ForkDraft) => hasTicket(source) || d.name.trim() !== "";

/** A title without the letter the server added to it: "Fix login (a)" is "Fix login". */
export const withoutLetter = (title: string) => title.replace(/\s*\([a-z]\)$/, "");

/** What the dialog sends for `source`. */
export function forkRequest(source: Clone, d: ForkDraft): CloneRequest {
  const message = d.message.trim();
  return {
    source: source.id,
    parent: d.sub ? forkParent(source) : undefined,
    linear: hasTicket(source)
      ? {
          workspace: source.linearWorkspace,
          ticket: source.linearTicket,
          ticketUrl: source.linearTicketUrl,
          branch: source.linearBranch,
          displayName: source.displayName && withoutLetter(source.displayName),
          label: source.linearLabel,
        }
      : { displayName: d.name.trim() },
    // Unchanged, the server keeps the source's pool
    group: d.group === sourceGroup(source) ? undefined : d.group,
    claudeAccount: d.claudeAccount || undefined,
    codexAccount: d.codexAccount || undefined,
    firstMessage: message || undefined,
    kickoff: message !== "",
    headless: d.headless,
    runStartupScript: d.runStartupScript,
    rebuild: d.rebuild,
  };
}
