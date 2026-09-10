import type {
  CollisionDetection,
  KeyboardCoordinateGetter,
} from "@dnd-kit/core";
import type { TreeDragItem, TreeDropTarget } from "~/lib/groupTreeDrag";

export const itemId = (item: TreeDragItem) => JSON.stringify(["item", item]);
export const targetId = (target: TreeDropTarget) =>
  JSON.stringify(["slot", target]);

/** Layout bounds include content hidden by scroll or overflow. Only the visible
 * intersection can receive a pointer drop. Keyboard scrolling is handled by its sensor. */
function visibleAt(
  element: HTMLElement | null,
  point: { x: number; y: number },
): boolean {
  if (!element) return false;
  const view = element.ownerDocument.defaultView;
  if (
    !view ||
    point.x < 0 ||
    point.y < 0 ||
    point.x > view.innerWidth ||
    point.y > view.innerHeight
  )
    return false;
  for (
    let parent = element.parentElement;
    parent;
    parent = parent.parentElement
  ) {
    const style = view.getComputedStyle(parent);
    const rect = parent.getBoundingClientRect();
    if (
      style.overflowX !== "visible" &&
      (point.x < rect.left || point.x > rect.right)
    )
      return false;
    if (
      style.overflowY !== "visible" &&
      (point.y < rect.top || point.y > rect.bottom)
    )
      return false;
  }
  return true;
}

/** Pointer drops must stay inside the editor (and, for accounts, inside a group).
 * Pick the nearest insertion boundary there. The unchanged layout prevents the
 * feedback loop where moving a row changes which target is under the pointer. */
export const treeCollision: CollisionDetection = ({
  active,
  pointerCoordinates,
  collisionRect,
  droppableContainers,
  droppableRects,
}) => {
  const item = active.data.current?.item as TreeDragItem | undefined;
  if (!item) return [];
  const point = pointerCoordinates ?? {
    x: collisionRect.left + collisionRect.width / 2,
    y: collisionRect.top + collisionRect.height / 2,
  };
  const region = droppableContainers.find((container) => {
    const data = container.data.current;
    if (item.kind === "group" ? !data?.treeRegion : !data?.groupRegion)
      return false;
    const rect = droppableRects.get(container.id);
    return (
      rect &&
      point.x >= rect.left &&
      point.x <= rect.right &&
      point.y >= rect.top &&
      point.y <= rect.bottom &&
      (!pointerCoordinates || visibleAt(container.node.current, point))
    );
  });
  if (!region) return [];
  const slots = droppableContainers.flatMap((container) => {
    const target = container.data.current?.target as TreeDropTarget | undefined;
    const rect = droppableRects.get(container.id);
    if (!target || !rect || target.kind !== item.kind) return [];
    if (
      target.kind === "member" &&
      target.groupId !== region.data.current?.groupRegion
    )
      return [];
    return [
      {
        id: container.id,
        distance: Math.abs(point.y - (rect.top + rect.height / 2)),
      },
    ];
  });
  slots.sort((a, b) => a.distance - b.distance);
  return slots.length ? [{ id: slots[0].id }] : [];
};

/** Keyboard users visit the same insertion boundaries as pointer users, including
 * empty groups and either end of every list. dnd-kit owns scrolling and Escape. */
export const treeKeyboardCoordinates: KeyboardCoordinateGetter = (
  event,
  { currentCoordinates, context },
) => {
  if (event.code !== "ArrowDown" && event.code !== "ArrowUp") return;
  event.preventDefault();
  const item = context.active?.data.current?.item as TreeDragItem | undefined;
  const rect = context.collisionRect;
  if (!item || !rect) return;
  const center = {
    x: rect.left + rect.width / 2,
    y: rect.top + rect.height / 2,
  };
  const slots = context.droppableContainers
    .getEnabled()
    .flatMap((container) => {
      const target = container.data.current?.target as
        | TreeDropTarget
        | undefined;
      const slot = context.droppableRects.get(container.id);
      if (!target || !slot || target.kind !== item.kind) return [];
      return [
        {
          id: container.id,
          x: slot.left + slot.width / 2,
          y: slot.top + slot.height / 2,
        },
      ];
    })
    .sort((a, b) => a.y - b.y);
  const down = event.code === "ArrowDown";
  const current = slots.findIndex((slot) => slot.id === context.over?.id);
  // Advance from the selected boundary, not the animated overlay. Its measured
  // center can lag behind a key press and otherwise select the same slot again.
  const next =
    current >= 0
      ? slots[current + (down ? 1 : -1)]
      : slots
          .filter((slot) =>
            down ? slot.y > center.y + 1 : slot.y < center.y - 1,
          )
          .sort((a, b) => (down ? a.y - b.y : b.y - a.y))[0];
  if (!next) return;
  return {
    x: currentCoordinates.x + next.x - center.x,
    y: currentCoordinates.y + next.y - center.y,
  };
};
