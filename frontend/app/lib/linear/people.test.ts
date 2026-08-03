// Who the assignee picker offers, out of an answer nothing type-checked. Three rules decide
// it, and each one is wrong in a way no type would catch: who is dropped, who is you, and who
// comes first.
import { expect, test } from "bun:test";

import { defaultAssignee, peopleFromResponse } from "./people";

/** The shape the real query answers with. Field names verified against api.linear.app. */
function response(members: unknown[], viewerId = "me-uuid") {
  return { teams: { nodes: [{ members: { nodes: members } }] }, viewer: { id: viewerId } };
}

test("the viewer comes first and is marked, the rest follow by name", () => {
  const people = peopleFromResponse(
    response([
      { id: "sam", name: "Sam Okafor", email: "sam@example.com", active: true },
      { id: "me-uuid", name: "Alex Rivera", email: "alex@example.com", active: true },
      { id: "jordan", name: "Jordan Blake", email: "jordan@example.com", active: true },
    ]),
  );

  expect(people.map((p) => p.name)).toEqual(["Alex Rivera", "Jordan Blake", "Sam Okafor"]);
  expect(people.map((p) => p.isViewer)).toEqual([true, false, false]);
});

// Linear will refuse the assignment, so offering the row is worse than a shorter list.
test("a deactivated member is dropped", () => {
  const people = peopleFromResponse(
    response([
      { id: "gone", name: "Gone Away", email: "gone@example.com", active: false },
      { id: "me-uuid", name: "Alex Rivera", email: "alex@example.com", active: true },
    ]),
  );

  expect(people.map((p) => p.id)).toEqual(["me-uuid"]);
});

test("a member with no name falls back to their email, and one with neither is dropped", () => {
  const people = peopleFromResponse(
    response([
      { id: "a", displayName: "aliased", email: "a@example.com", active: true },
      { id: "b", email: "b@example.com", active: true },
      { id: "c", active: true },
    ]),
  );

  expect(people.map((p) => p.name)).toEqual(["aliased", "b@example.com"]);
});

// A key whose owner is not a member of the team it is querying still gets a usable list. The
// column will not show what it creates, which the dialog says rather than the mapping.
test("nobody is the viewer when the viewer is not in the team", () => {
  const people = peopleFromResponse(
    response([{ id: "sam", name: "Sam Okafor", email: "s@example.com", active: true }]),
  );

  expect(people.map((p) => p.isViewer)).toEqual([false]);
});

test("a team Linear does not know, and a malformed answer, are both empty rather than thrown", () => {
  expect(peopleFromResponse({ teams: { nodes: [] }, viewer: { id: "me" } })).toEqual([]);
  expect(peopleFromResponse(null)).toEqual([]);
  expect(peopleFromResponse({ teams: "nonsense" })).toEqual([]);
});

// --- the default -------------------------------------------------------------

test("the dialog starts on you, and on the first member when the key owns no user", () => {
  const me = { id: "me-uuid", name: "Alex", email: "a@example.com", isViewer: true };
  const sam = { id: "sam", name: "Sam", email: "s@example.com", isViewer: false };

  expect(defaultAssignee([sam, me])).toBe("me-uuid");
  expect(defaultAssignee([sam])).toBe("sam");
  expect(defaultAssignee([])).toBe("");
});
