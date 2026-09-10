import { moveMember, type TreeGroup } from "./groupTree";

/** Editor-only identity. Names and list positions are not identities. */
export interface EditorGroup extends TreeGroup {
  id: string;
}

export type TreeDragItem =
  | { kind: "group"; groupId: string }
  | { kind: "member"; groupId: string; email: string };

/** An insertion boundary in the unchanged list, not the row under the pointer. */
export type TreeDropTarget =
  | { kind: "group"; index: number }
  | { kind: "member"; groupId: string; index: number };

/** Apply exactly one drop. Invalid, duplicate and unchanged drops return null.
 * The caller keeps the original tree mounted for the whole drag and calls this only
 * on release. Repeated hover events therefore cannot move or lose a membership. */
export function applyTreeDrop(
  groups: EditorGroup[],
  item: TreeDragItem,
  target: TreeDropTarget,
): EditorGroup[] | null {
  const from = groups.findIndex((group) => group.id === item.groupId);
  if (from < 0 || item.kind !== target.kind || !Number.isInteger(target.index))
    return null;
  if (item.kind === "group" && target.kind === "group") {
    if (target.index < 0 || target.index > groups.length) return null;
    const to = target.index > from ? target.index - 1 : target.index;
    if (from === to) return null;
    const next = [...groups];
    const [group] = next.splice(from, 1);
    next.splice(to, 0, group);
    return next;
  }
  if (item.kind !== "member" || target.kind !== "member") return null;
  const to = groups.findIndex((group) => group.id === target.groupId);
  const index = groups[from].accounts.indexOf(item.email);
  if (
    to < 0 ||
    index < 0 ||
    target.index < 0 ||
    target.index > groups[to].accounts.length
  )
    return null;
  if (from === to && (target.index === index || target.index === index + 1))
    return null;
  const next = moveMember(
    groups,
    { group: from, index },
    { group: to, index: target.index },
  );
  return next?.map((group, i) => ({ ...group, id: groups[i].id })) ?? null;
}

export function isDuplicateDrop(
  groups: EditorGroup[],
  item: TreeDragItem,
  target: TreeDropTarget,
): boolean {
  return (
    item.kind === "member" &&
    target.kind === "member" &&
    item.groupId !== target.groupId &&
    !!groups
      .find((group) => group.id === target.groupId)
      ?.accounts.includes(item.email)
  );
}
