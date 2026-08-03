// Who a ticket can be assigned to, as the container's Linear lookup answers.
//
// The emails match the account fixtures, so the operator a story assigns a ticket to is the
// same person whose Claude usage the rail draws. Alex is the key's own owner in every team,
// because the key is the operator's and a fleet's keys are all their own accounts.

import type { TicketPerson } from "~/lib/linear/people";

export function makePerson(overrides: Partial<TicketPerson> = {}): TicketPerson {
  const email = overrides.email ?? "alex@example.com";
  return {
    id: `user_${email.split("@")[0]}`,
    name: "Alex Rivera",
    email,
    isViewer: false,
    ...overrides,
  };
}

/** A team's members, viewer first, which is the order the real lookup sorts them into.
 *
 *  Membership varies by team, because that is the fact the picker depends on: switching team
 *  in the dialog swaps this list and puts the assignee back on you. `we` has three people,
 *  `dev` two, and anything else just you. */
export function makeTeamPeople(team = "we"): TicketPerson[] {
  const me = makePerson({ isViewer: true });
  const sam = makePerson({
    email: "sam@example.com",
    name: "Sam Okafor",
  });
  const jordan = makePerson({
    email: "jordan@example.com",
    name: "Jordan Blake",
  });
  if (team === "dev") return [me, jordan];
  if (team === "we" || team === "frontend") return [me, jordan, sam];
  return [me];
}
