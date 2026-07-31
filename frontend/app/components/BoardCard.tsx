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

/** The same hairline the side panel's cards carry, so everything card-shaped on screen is
 *  outlined the one way. It stays put when a card lifts: a border that darkens mid-drag
 *  reads as the card changing, not as it rising. */
const OUTLINE = "border border-slate-900/10 dark:border-white/10";

/** Wide and faint rather than tight and dark, so the card sits on the column instead of
 *  being cut out of it. A lifted card is told apart by how far its shadow spreads.
 *
 *  Dark mode runs the same geometry at roughly five times the alpha, in black rather than
 *  slate. A shadow is only visible as the difference between it and the surface under it,
 *  and against a slate-800 column the light-mode values come out to nothing. */
const RESTING =
  "shadow-[0_1px_6px_rgb(15_23_42_/_0.04),0_4px_16px_rgb(15_23_42_/_0.05)] dark:shadow-[0_1px_6px_rgb(0_0_0_/_0.22),0_4px_16px_rgb(0_0_0_/_0.26)]";
const LIFTED =
  "shadow-[0_4px_16px_rgb(15_23_42_/_0.06),0_16px_48px_rgb(15_23_42_/_0.10)] dark:shadow-[0_4px_16px_rgb(0_0_0_/_0.3),0_16px_48px_rgb(0_0_0_/_0.4)]";

/** The card body, no drag wiring. Used for the drag overlay, where dnd-kit positions the
 *  node itself. `children` are this clone's sub-clone rows, drawn inside the same frame. */
export function BoardCardBody({
  lifted = false,
  children,
  ...card
}: SidebarCloneProps & { lifted?: boolean; children?: ReactNode }) {
  return (
    <div
      className={`overflow-hidden rounded-lg bg-white dark:bg-slate-900 ${OUTLINE} ${
        lifted ? LIFTED : RESTING
      }`}
    >
      <SidebarClone {...card} />
      {children}
    </div>
  );
}

export function BoardCard({
  id,
  children,
  ...card
}: SidebarCloneProps & { id: string; children?: ReactNode }) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id,
    // A clone with an operation in flight (delete, or a fresh clone finishing its setup)
    // stays put until it settles.
    disabled: card.op?.status === "running",
  });
  const style: CSSProperties = {
    transform: CSS.Translate.toString(transform),
    transition,
    // The original keeps its slot in the column as a hole while the overlay follows the
    // pointer, so the cards below do not jump twice.
    opacity: isDragging ? 0 : 1,
  };

  return (
    <div ref={setNodeRef} style={style}>
      <BoardCardBody {...card} dragAttributes={attributes} dragListeners={listeners}>
        {children}
      </BoardCardBody>
    </div>
  );
}
