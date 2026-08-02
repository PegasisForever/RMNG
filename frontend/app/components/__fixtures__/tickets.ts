// What the ticket column draws: the union of every configured Linear key's own issues.
//
// `WE-142` and `DEV-88` are deliberately here AND on a clone in `hosts`, and `DEV-104`
// appears twice as two presets sharing one key would return it. `openTickets` is what drops
// all three, so a story that skips the filter shows the bug rather than hiding it.

import type { LinearTicket } from "~/lib/tickets";

export function makeTicket(overrides: Partial<LinearTicket> = {}): LinearTicket {
  return {
    id: "WE-100",
    title: "Untitled issue",
    url: "https://linear.app/pegasis/issue/WE-100/untitled-issue",
    state: "todo",
    team: "WE",
    labels: [],
    children: [],
    ...overrides,
  };
}

/** Everything filled in: priority, assignee, estimate, due date, labels, a description, and
 *  three sub-issues with one already done. */
export function makeTicketDetailed(overrides: Partial<LinearTicket> = {}): LinearTicket {
  return makeTicket({
    id: "WE-301",
    title: "Encoder drops frames when a second monitor is hot-plugged",
    url: "https://linear.app/pegasis/issue/WE-301/encoder-drops-frames",
    // Linear sent its own branch name for this one, handle prefix and all; the rest fall
    // back to the derived shape.
    branchName: "alex/we-301-encoder-drops-frames-when-a-second-monitor",
    assignee: "Alex",
    dueDate: "2026-08-14",
    estimate: 3,
    description:
      "Hot-plugging a second monitor mid-session drops roughly 40 frames before the encoder settles.\n\n" +
      "## What happens\n\n" +
      "`vapostproc` renegotiates caps when the monitor set changes, and the encoder is torn down and rebuilt " +
      "while frames are still arriving. Those frames are dropped rather than queued.\n\n" +
      "## What it should do\n\n" +
      "1. Hold the incoming frames while the caps settle.\n" +
      "2. Rebuild the encoder against the new layout.\n" +
      "3. Drain the held frames.",
    children: [
      {
        id: "WE-302",
        title: "Reproduce with a scripted hot-plug",
        url: "https://linear.app/pegasis/issue/WE-302/reproduce",
        state: "done",
      },
      {
        id: "WE-303",
        title: "Hold frames across a caps renegotiation",
        url: "https://linear.app/pegasis/issue/WE-303/hold-frames",
        state: "in_progress",
      },
      {
        id: "WE-304",
        title: "Add a hot-plug case to the capture self-test",
        url: "https://linear.app/pegasis/issue/WE-304/self-test",
        state: "todo",
      },
    ],
    labels: [
      { name: "Bug", color: "#eb5757" },
      { name: "Video", color: "#0f7488" },
    ],
    priority: 1,
    ...overrides,
  });
}

/** A sub-issue itself: it has a parent and no sub-issues of its own. */
export function makeTicketSubIssue(overrides: Partial<LinearTicket> = {}): LinearTicket {
  return makeTicket({
    id: "WE-288",
    title: "Board columns should remember their scroll position",
    url: "https://linear.app/pegasis/issue/WE-288/board-columns-scroll",
    labels: [{ name: "Feature", color: "#bb87fc" }],
    assignee: "Alex",
    description:
      "Scrolling a column, selecting a clone, and coming back puts you at the top again.\n\n" +
      "Store the offset per column and restore it on mount.",
    parent: {
      id: "WE-280",
      title: "Board polish",
      url: "https://linear.app/pegasis/issue/WE-280/board-polish",
      state: "in_progress",
    },
    state: "in_progress",
    priority: 2,
    ...overrides,
  });
}

/** Nothing but a title: every other property is unset. */
export function makeTicketBare(overrides: Partial<LinearTicket> = {}): LinearTicket {
  return makeTicket({
    id: "DEV-97",
    title: "Document the bastion port in the SSH panel",
    url: "https://linear.app/pegasis/issue/DEV-97/document-bastion-port",
    labels: [{ name: "Docs", color: "#5e6ad2" }],
    team: "DEV",
    ...overrides,
  });
}

/** The issue two presets sharing one key would both return. `linearTickets` carries it twice
 *  so `openTickets`'s deduplication is visible rather than assumed. */
export function makeTicketDuplicated(overrides: Partial<LinearTicket> = {}): LinearTicket {
  return makeTicket({
    id: "DEV-104",
    title: "Retry the usage poll on a 429 instead of dropping the window",
    url: "https://linear.app/pegasis/issue/DEV-104/retry-usage-poll",
    labels: [
      { name: "Feature", color: "#bb87fc" },
      { name: "Voice", color: "#4ea7fc" },
    ],
    team: "DEV",
    priority: 3,
    ...overrides,
  });
}

/** The ticket `cloneWorking` was made from. `openTickets` drops it from the column, so it is
 *  only ever drawn as a clone's own ticket: the panel in that clone's notes card, and the
 *  title on its card. */
export function makeTicketCloned(overrides: Partial<LinearTicket> = {}): LinearTicket {
  return makeTicket({
    id: "WE-142",
    title: "Normalize sidebar CPU to % of allowance",
    url: "https://linear.app/pegasis/issue/WE-142/normalize-sidebar-cpu",
    description:
      "The sidebar reports raw container CPU, so a clone pinned to two cores reads 200%.\n\n" +
      "Divide by the host's core count and show one figure that means the same thing on " +
      "every machine.",
    labels: [{ name: "Frontend", color: "#4ea7fc" }],
    state: "in_progress",
    priority: 2,
    ...overrides,
  });
}

export const ticketDetailed: LinearTicket = makeTicketDetailed();
export const ticketSubIssue: LinearTicket = makeTicketSubIssue();
export const ticketBare: LinearTicket = makeTicketBare();
export const ticketCloned: LinearTicket = makeTicketCloned();

export const linearTickets: LinearTicket[] = [
  ticketDetailed,
  ticketSubIssue,
  makeTicketDuplicated(),
  ticketBare,
  // The same issue again, as a second key carrying the same account would return it.
  makeTicketDuplicated(),
  // Already cloned (see `cloneWorking` / `cloneIdle`), so the filter drops both.
  ticketCloned,
  makeTicket({
    id: "DEV-88",
    title: "Wire up the pull-template wizard",
    url: "https://linear.app/pegasis/issue/DEV-88/pull-template-wizard",
    team: "DEV",
  }),
];
