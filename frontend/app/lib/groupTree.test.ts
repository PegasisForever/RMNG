import { expect, test } from "bun:test";
import {
  addReference,
  moveMember,
  orphanedAccounts,
  removeMember,
  reorderGroups,
  type TreeGroup,
} from "./groupTree";

const groups = (): TreeGroup[] => [
  { name: "A", accounts: ["a@x", "b@x"] },
  { name: "B", accounts: ["c@x"] },
];

test("reordering groups keeps members attached", () => {
  const out = reorderGroups(groups(), 0, 1);

  expect(out.map((g) => g.name)).toEqual(["B", "A"]);
  expect(out[1].accounts).toEqual(["a@x", "b@x"]);
});

test("moving a member within its group reorders", () => {
  const out = moveMember(groups(), { group: 0, index: 0 }, { group: 0, index: 2 });

  expect(out?.[0].accounts).toEqual(["b@x", "a@x"]);
  expect(out?.[1].accounts).toEqual(["c@x"]);
});

test("moving a member across groups relocates it (no copy left behind)", () => {
  const out = moveMember(groups(), { group: 0, index: 0 }, { group: 1, index: 1 });

  expect(out?.[0].accounts).toEqual(["b@x"]);
  expect(out?.[1].accounts).toEqual(["c@x", "a@x"]);
});

test("dropping onto a group that already holds the email is a no-op", () => {
  const withDup: TreeGroup[] = [
    { name: "A", accounts: ["a@x"] },
    { name: "B", accounts: ["a@x", "c@x"] },
  ];

  expect(moveMember(withDup, { group: 0, index: 0 }, { group: 1, index: 0 })).toBeNull();
  expect(addReference(withDup, 1, "a@x")).toBeNull();
});

test("the clone-reference button appends without disturbing order", () => {
  const out = addReference(groups(), 1, "b@x");

  expect(out?.[1].accounts).toEqual(["c@x", "b@x"]);
  expect(out?.[0].accounts).toEqual(["a@x", "b@x"]);
});

test("removing a membership reports whether the account is orphaned", () => {
  // "a@x" sits only in A, so dropping it there orphans it (routes to delete).
  expect(removeMember(groups(), 0, "a@x")).toEqual({
    groups: [
      { name: "A", accounts: ["b@x"] },
      { name: "B", accounts: ["c@x"] },
    ],
    orphaned: true,
  });
  // Still referenced in A after dropping it from B → plain draft edit.
  const shared: TreeGroup[] = [
    { name: "A", accounts: ["a@x"] },
    { name: "B", accounts: ["a@x", "c@x"] },
  ];
  expect(removeMember(shared, 1, "a@x").orphaned).toBe(false);
});

test("the save sweep finds exactly the unclaimed accounts", () => {
  expect(orphanedAccounts(groups(), ["a@x", "b@x", "c@x", "gone@x"])).toEqual(["gone@x"]);
});
