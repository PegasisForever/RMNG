// One clone as a board card. The card body is the same SidebarClone the list layout uses,
// so a card keeps every action and metric it had in the sidebar; this wraps it in the
// rounded, shadowed frame a board column expects and makes it draggable.
//
// A card holds one clone plus, through `children`, its sub clones. One frame around the whole
// group is what says they belong to the clone that spawned them — separate cards would read
// as peers, and a gap between them as unrelated work.
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import type { CSSProperties, ReactNode } from "react";

import { SidebarClone, type SidebarCloneProps } from "~/components/SidebarClone";
import { GLASS_FILL, GLASS_OUTLINE, GLASS_SHADOW_LIFTED } from "~/lib/glass";

/** The same hairline the side panel's cards carry, so everything card-shaped on screen is
 *  outlined the one way. It stays put when a card lifts: a border that darkens mid-drag
 *  reads as the card changing, not as it rising. */
const OUTLINE = GLASS_OUTLINE;

/** A card at rest sits closer to the column than a floating panel does, so its shadow is
 *  tighter than the glass one. Lifting it swaps in the shared lifted shadow, and how far
 *  that spreads is what tells the two apart.
 *
 *  Dark mode runs the same geometry at roughly five times the alpha, in black rather than
 *  slate. A shadow is only visible as the difference between it and the surface under it,
 *  and against a slate-800 column the light-mode values come out to nothing. */
const RESTING =
  "shadow-[0_1px_6px_rgb(15_23_42_/_0.04),0_4px_16px_rgb(15_23_42_/_0.05)] dark:shadow-[0_1px_6px_rgb(0_0_0_/_0.22),0_4px_16px_rgb(0_0_0_/_0.26)]";
const LIFTED = GLASS_SHADOW_LIFTED;

/** A resting card is opaque, so it hides the column behind it. A lifted one is the glass
 *  every floating thing on this screen is made of: a card in the air is over the board
 *  rather than in it. */
const RESTING_FILL = "bg-white dark:bg-slate-900";

/** The frame every board card wears: rounded, outlined, and shadowed to sit on the column.
 *  Exported because the ticket cards are the same object at rest, and two copies of these
 *  values is how the board ends up with two kinds of card that almost match.
 *
 *  `data-lifted` is what lets the card's contents answer the drag. The fill lives on this
 *  frame, but the clone row paints its own on top, so the row has to drop it while the card
 *  is in the air. One attribute here reaches the row and every sub-clone row under it. */
export function CardFrame({ lifted = false, children }: { lifted?: boolean; children: ReactNode }) {
  return (
    <div
      data-lifted={lifted ? "" : undefined}
      // A card is a drag handle, so nothing in it is selectable. Dragging one otherwise
      // paints a selection across its title on the way, and the text that comes with a card
      // is a label, not content anybody copies out by hand. The ⋮ menu carries the copy
      // actions for the parts that are worth copying.
      className={`group/card select-none overflow-hidden rounded-lg ${lifted ? GLASS_FILL : RESTING_FILL} ${OUTLINE} ${
        lifted ? LIFTED : RESTING
      }`}
    >
      {children}
    </div>
  );
}

/** The card body, no drag wiring. Used for the drag overlay, where dnd-kit positions the
 *  node itself. `children` are this clone's sub-clone rows, drawn inside the same frame. */
export function BoardCardBody({
  lifted = false,
  children,
  ...card
}: SidebarCloneProps & { lifted?: boolean; children?: ReactNode }) {
  return (
    <CardFrame lifted={lifted}>
      <SidebarClone {...card} />
      {children}
    </CardFrame>
  );
}

export function BoardCard({
  id,
  children,
  ...card
}: SidebarCloneProps & { id: string; children?: ReactNode }) {
  const { listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id,
    // A clone with an operation in flight (delete, or a fresh clone finishing its setup)
    // stays put until it settles.
    disabled: card.op?.status === "running",
  });
  // Pointer drag only. The whole card is the drag activator, so dnd-kit's keyboard activator
  // lifts the card on any Space or Enter that reaches it, including a click that left focus on
  // the card. Dropping the handler is what disables that; dnd-kit's `attributes` go with it,
  // since they are what advertise the keyboard drag (`tabIndex`, `role`, and the
  // `aria-describedby` that points at "press the space bar to pick up").
  const { onKeyDown: _liftOnSpace, ...dragListeners } = listeners ?? {};
  const style: CSSProperties = {
    transform: CSS.Translate.toString(transform),
    transition,
    // The original keeps its slot in the column as a hole while the overlay follows the
    // pointer, so the cards below do not jump twice.
    opacity: isDragging ? 0 : 1,
  };

  return (
    <div ref={setNodeRef} style={style}>
      <BoardCardBody {...card} dragListeners={dragListeners}>
        {children}
      </BoardCardBody>
    </div>
  );
}
