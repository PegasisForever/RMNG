import { expect, test } from "bun:test";
import type { GroupMember } from "./wire/GroupMember";
import {
  applyTreeDrop,
  isDuplicateDrop,
  type EditorGroup,
  type TreeDragItem,
  type TreeDropTarget,
} from "./groupTreeDrag";

const m = (email: string, provider?: GroupMember["provider"]): GroupMember =>
  provider ? { email, provider } : { email };
const emails = (ms: GroupMember[]): string[] => ms.map((x) => x.email);
const tree = (): EditorGroup[] => [
  { id: "a", name: "same", accounts: [m("a@x"), m("b@x"), m("c@x")] },
  { id: "b", name: "same", accounts: [m("d@x")] },
  { id: "c", name: "", accounts: [m("a@x")] },
  { id: "d", name: "", accounts: [] },
];
const member: TreeDragItem = { kind: "member", groupId: "a", email: "a@x" };

test("a member moves once into the selected group, retaining unrelated references", () => {
  const input = tree();
  const original = structuredClone(input);
  const output = applyTreeDrop(input, member, {
    kind: "member",
    groupId: "b",
    index: 1,
  });
  expect(output?.map((group) => emails(group.accounts))).toEqual([
    ["b@x", "c@x"],
    ["d@x", "a@x"],
    ["a@x"],
    [],
  ]);
  expect(output?.map((group) => group.id)).toEqual(["a", "b", "c", "d"]);
  expect(input).toEqual(original);
  output?.forEach((group, i) => {
    expect(group).not.toBe(input[i]);
    expect(group.accounts).not.toBe(input[i].accounts);
  });
});

test("member insertion boundaries work in both directions and at either end", () => {
  expect(
    applyTreeDrop(tree(), member, {
      kind: "member",
      groupId: "a",
      index: 3,
    })?.[0].accounts.map((x) => x.email),
  ).toEqual(["b@x", "c@x", "a@x"]);
  expect(
    applyTreeDrop(
      tree(),
      { ...member, email: "c@x" },
      { kind: "member", groupId: "a", index: 0 },
    )?.[0].accounts.map((x) => x.email),
  ).toEqual(["c@x", "a@x", "b@x"]);
  expect(
    applyTreeDrop(tree(), member, {
      kind: "member",
      groupId: "b",
      index: 0,
    })?.[1].accounts.map((x) => x.email),
  ).toEqual(["a@x", "d@x"]);
  expect(
    applyTreeDrop(tree(), member, {
      kind: "member",
      groupId: "d",
      index: 0,
    })?.[3].accounts.map((x) => x.email),
  ).toEqual(["a@x"]);
});

test("duplicate destinations are refused without deleting the source", () => {
  const input = tree();
  const target: TreeDropTarget = { kind: "member", groupId: "c", index: 0 };
  expect(isDuplicateDrop(input, member, target)).toBe(true);
  expect(applyTreeDrop(input, member, target)).toBeNull();
  expect(input).toEqual(tree());
});

test("adjacent boundaries of the source are no-ops", () => {
  for (const index of [0, 1]) {
    expect(
      applyTreeDrop(tree(), member, { kind: "member", groupId: "a", index }),
    ).toBeNull();
    expect(
      applyTreeDrop(
        tree(),
        { kind: "group", groupId: "a" },
        { kind: "group", index },
      ),
    ).toBeNull();
  }
});

test("groups retain identities and members through successive reorders, even with duplicate names", () => {
  const original = tree();
  const down = applyTreeDrop(
    original,
    { kind: "group", groupId: "a" },
    { kind: "group", index: 4 },
  );
  expect(down?.map((group) => group.id)).toEqual(["b", "c", "d", "a"]);
  expect(down?.[3]).toBe(original[0]);
  const up = applyTreeDrop(
    down!,
    { kind: "group", groupId: "a" },
    { kind: "group", index: 0 },
  );
  expect(up).toEqual(original);
  const moved = applyTreeDrop(down!, member, {
    kind: "member",
    groupId: "b",
    index: 1,
  });
  expect(moved?.map((group) => group.id)).toEqual(["b", "c", "d", "a"]);
  expect(emails(moved?.[0].accounts ?? [])).toEqual(["d@x", "a@x"]);
  expect(emails(moved?.[3].accounts ?? [])).toEqual(["b@x", "c@x"]);
});

test("invalid or stale drops cannot alter another item", () => {
  const cases: [TreeDragItem, TreeDropTarget][] = [
    [member, { kind: "group", index: 0 }],
    [
      { kind: "group", groupId: "missing" },
      { kind: "group", index: 0 },
    ],
    [
      { kind: "group", groupId: "a" },
      { kind: "group", index: 8 },
    ],
    [
      { ...member, email: "missing" },
      { kind: "member", groupId: "b", index: 0 },
    ],
    [member, { kind: "member", groupId: "missing", index: 0 }],
    [member, { kind: "member", groupId: "b", index: -1 }],
    [member, { kind: "member", groupId: "b", index: 9 }],
    [member, { kind: "member", groupId: "b", index: Number.NaN }],
    [member, { kind: "member", groupId: "b", index: 0.5 }],
    [member, { kind: "member", groupId: "b", index: Infinity }],
    [
      { ...member, groupId: "missing" },
      { kind: "member", groupId: "b", index: 0 },
    ],
    [
      { kind: "group", groupId: "a" },
      { kind: "group", index: -1 },
    ],
  ];
  for (const [item, target] of cases) {
    expect(applyTreeDrop(tree(), item, target)).toBeNull();
    expect(applyTreeDrop([], item, target)).toBeNull();
  }
});

test("every possible member drop preserves the membership multiset and leaves the input unchanged", () => {
  const input = tree();
  const original = structuredClone(input);
  for (const source of input)
    for (const { email } of source.accounts) {
      for (const destination of input)
        for (let index = 0; index <= destination.accounts.length; index++) {
          const output = applyTreeDrop(
            input,
            { kind: "member", groupId: source.id, email },
            { kind: "member", groupId: destination.id, index },
          );
          if (output) {
            expect(output.flatMap((group) => emails(group.accounts)).sort()).toEqual(
              input.flatMap((group) => emails(group.accounts)).sort(),
            );
            for (const group of output)
              expect(new Set(emails(group.accounts)).size).toBe(group.accounts.length);
          }
          expect(input).toEqual(original);
        }
    }
});

test("a moved row keeps its provider scope", () => {
  const input: EditorGroup[] = [
    { id: "a", name: "", accounts: [m("a@x", "claude")] },
    { id: "b", name: "", accounts: [] },
  ];
  const output = applyTreeDrop(
    input,
    { kind: "member", groupId: "a", email: "a@x" },
    { kind: "member", groupId: "b", index: 0 },
  );
  expect(output?.[1].accounts).toEqual([m("a@x", "claude")]);
  expect(output?.[0].accounts).toEqual([]);
});
