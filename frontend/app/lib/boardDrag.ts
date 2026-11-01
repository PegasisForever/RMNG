// Where a dragged clone card lands among the columns. Pure, so the same calls resolve a hover
// preview and the final drop, and a test can pin them without mounting anything.
import { arrayMove } from "@dnd-kit/sortable";

/** A column as drawn. Ids are column ids. */
export type Lane = { id: string; cloneIds: string[] };

/** The lane holding `cardId`, or the lane whose own id it is (dropping on a column's empty
 *  body reports the column). */
export function laneOf(lanes: Lane[], id: string): Lane | undefined {
  return lanes.find((lane) => lane.id === id) ?? lanes.find((lane) => lane.cloneIds.includes(id));
}

/** The arrangement with `activeId` moved to wherever `overId` currently sits. */
export function arrange(lanes: Lane[], activeId: string, overId: string): Lane[] {
  const from = lanes.find((lane) => lane.cloneIds.includes(activeId));
  const to = laneOf(lanes, overId);
  if (!from || !to) return lanes;

  const overIndex = to.cloneIds.indexOf(overId);
  if (from.id === to.id) {
    const fromIndex = from.cloneIds.indexOf(activeId);
    // Over its own column rather than a card ⇒ the end of the list.
    const toIndex = overIndex < 0 ? from.cloneIds.length - 1 : overIndex;
    if (toIndex === fromIndex) return lanes;
    return lanes.map((lane) =>
      lane.id === from.id
        ? { ...lane, cloneIds: arrayMove(lane.cloneIds, fromIndex, toIndex) }
        : lane,
    );
  }
  // Over the column itself rather than one of its cards ⇒ the end of the list.
  const insertAt = overIndex < 0 ? to.cloneIds.length : overIndex;
  return lanes.map((lane) => {
    if (lane.id === from.id) {
      return { ...lane, cloneIds: lane.cloneIds.filter((id) => id !== activeId) };
    }
    if (lane.id === to.id) {
      const cloneIds = [...lane.cloneIds];
      cloneIds.splice(insertAt, 0, activeId);
      return { ...lane, cloneIds };
    }
    return lane;
  });
}

/** How far the pointer must travel, in px, before the columns will rearrange again.
 *
 *  Under the mouse sensor's own 5px activation distance, so it costs nothing a drag has not
 *  already paid for, and small enough that reordering still feels immediate. */
export const REARRANGE_MIN_TRAVEL = 4;

/** `next`, or `prev` itself when the two arrangements hold the same ids in the same order.
 *
 *  Identity is load-bearing. dnd-kit re-measures every droppable whenever this state changes,
 *  and each measurement fires `onDragOver` again — so an arrangement that allocates a fresh
 *  array while describing the same layout keeps that cycle alive until React gives up with
 *  "Maximum update depth exceeded". `arrange` already returns its input untouched when a move
 *  is a no-op within one lane; this covers the cross-lane branch, which always allocates. */
export function settle(prev: Lane[], next: Lane[]): Lane[] {
  if (prev === next) return prev;
  if (prev.length !== next.length) return next;
  const unchanged = prev.every((lane, i) => {
    const other = next[i];
    return (
      lane.id === other.id &&
      lane.cloneIds.length === other.cloneIds.length &&
      lane.cloneIds.every((id, j) => id === other.cloneIds[j])
    );
  });
  return unchanged ? prev : next;
}
