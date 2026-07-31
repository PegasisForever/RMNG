// One clone as a board card. The card body is the same SidebarClone the list layout uses,
// so a card keeps every action and metric it had in the sidebar; this wraps it in the
// rounded, shadowed frame a board column expects and makes it draggable.
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import type { CSSProperties } from "react";

import { SidebarClone, type SidebarCloneProps } from "~/components/SidebarClone";

/** The card body, no drag wiring. Used for the drag overlay, where dnd-kit positions the
 *  node itself. */
export function BoardCardBody({ lifted = false, ...card }: SidebarCloneProps & { lifted?: boolean }) {
  return (
    <div
      className={`overflow-hidden rounded-lg border bg-white dark:bg-slate-900 ${
        lifted
          ? "border-slate-300 shadow-xl dark:border-slate-600"
          : "border-slate-200 shadow-sm dark:border-slate-700"
      }`}
    >
      <SidebarClone {...card} />
    </div>
  );
}

export function BoardCard({ id, ...card }: SidebarCloneProps & { id: string }) {
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
      <BoardCardBody {...card} dragAttributes={attributes} dragListeners={listeners} />
    </div>
  );
}
