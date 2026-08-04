// One board column: a header (title, card count, and for operator-made columns a rename
// and a delete) over a droppable, scrollable card list.
//
// The header sits on the board, outside the recess. What is carved out is the well the cards
// live in, and a title floating inside it would claim to be one of them.
//
// The list stays a drop target when it is empty, which is what `useDroppable` on the body
// is for: with only the cards registered, an empty column could never receive the first one.
import { useDroppable } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { Check, Plus, X } from "lucide-react";
import { type ReactNode, useState } from "react";

/** The shared column chrome. A board column is a shell of a fixed width, a header row, and
 *  a well the cards sit in; the ticket column is the same object with different contents, so
 *  all three live here and neither side owns a private copy of the measurements. */
export function ColumnShell({ children }: { children: ReactNode }) {
  return <section className="flex w-80 shrink-0 flex-col">{children}</section>;
}

export function ColumnHeader({ children }: { children: ReactNode }) {
  return <header className="flex shrink-0 items-center gap-2 px-1 pb-2">{children}</header>;
}

/** The title's own type, without the flex behavior that decides how much of the header row it
 *  takes. A title with something beside it inside the header (the ticket column's workspace
 *  menu) wraps the pair in the flexing box and styles the text with this. */
export const COLUMN_TITLE_TEXT =
  "min-w-0 truncate text-sm font-semibold text-slate-700 dark:text-slate-200";

/** The header's title styling, as a class rather than a component: one column's title opens
 *  a rename on double-click and the other's does nothing, so the element itself differs. */
export const COLUMN_TITLE = `flex-1 ${COLUMN_TITLE_TEXT}`;

/** The well the cards sit in, carved into the board rather than laid on it: a dark inner
 *  shadow across the top where a recess would fall into shadow, and a one-pixel light line
 *  along the bottom where its far wall would catch the light. The cards then read as sitting
 *  in the column, which is the one thing their own outer shadow cannot say by itself.
 *
 *  Light only. A recess reads by being darker than what surrounds it, and in dark mode the
 *  column is already the lighter of the two — the same shadow there just muddies its top
 *  edge. The fill carries the separation on its own.
 *
 *  No hover tint on the drop target. The cards themselves already part around the pointer to
 *  show where the drop lands, so a colour wash on the column only repeats it, and it repeats
 *  it a whole column wide. */
export function ColumnWell({
  innerRef,
  empty = null,
  children,
}: {
  /** A drop target's node ref. Absent ⇒ the well receives nothing, which is what the ticket
   *  column wants: a ticket leaves it by becoming a clone, not by being dropped back. */
  innerRef?: (node: HTMLElement | null) => void;
  /** Shown under the children when there is nothing in the well. */
  empty?: string | null;
  children: ReactNode;
}) {
  return (
    <div
      ref={innerRef}
      className="min-h-0 flex-1 space-y-2 overflow-y-auto rounded-xl bg-slate-100/60 p-2 shadow-[inset_0_1px_3px_rgb(15_23_42_/_0.045),inset_0_1px_10px_rgb(15_23_42_/_0.03),inset_0_-1px_0_rgb(255_255_255_/_0.5)] dark:bg-slate-800/70 dark:shadow-none"
    >
      {children}
      {empty ? (
        <p className="rounded-lg border border-dashed border-slate-300 p-4 text-center text-xs text-slate-400 dark:border-slate-600 dark:text-slate-500">
          {empty}
        </p>
      ) : null}
    </div>
  );
}

export interface BoardColumnPanelProps {
  id: string;
  title: string;
  /** Card order, for the sortable context. Must match the cards rendered as children. */
  cloneIds: string[];
  /** Shown when the column holds no cards. */
  empty: string;
  onRename?: (title: string) => void;
  /** Create a clone that lands in this column. Absent ⇒ no button. */
  onNewClone?: () => void;
  /** A clone is already being provisioned; the button waits it out. */
  newCloneBusy?: boolean;
  children: ReactNode;
}

export function BoardColumnPanel({
  id,
  title,
  cloneIds,
  empty,
  onRename,
  onNewClone,
  newCloneBusy = false,
  children,
}: BoardColumnPanelProps) {
  const { setNodeRef } = useDroppable({ id });
  const [renaming, setRenaming] = useState<string | null>(null);

  return (
    <ColumnShell>
      <ColumnHeader>
        {renaming === null ? (
          <>
            <h2
              onDoubleClick={() => setRenaming(title)}
              title="Double-click to rename"
              className={COLUMN_TITLE}
            >
              {title}
            </h2>
            {onNewClone ? (
              <button
                type="button"
                onClick={onNewClone}
                disabled={newCloneBusy}
                title={`Create a clone in ${title}`}
                className="flex shrink-0 items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-slate-500 hover:bg-slate-200 disabled:opacity-40 dark:text-slate-400 dark:hover:bg-slate-700"
              >
                <Plus className="size-3.5" />
                New clone
              </button>
            ) : null}
          </>
        ) : (
          <form
            className="flex min-w-0 flex-1 items-center gap-1"
            onSubmit={(e) => {
              e.preventDefault();
              const next = renaming.trim();
              if (next) onRename?.(next);
              setRenaming(null);
            }}
          >
            <input
              autoFocus
              value={renaming}
              onChange={(e) => setRenaming(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") setRenaming(null);
              }}
              aria-label="Column name"
              className="min-w-0 flex-1 rounded-md border border-slate-300 bg-white px-2 py-1 text-sm text-slate-900 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-900 dark:text-slate-100"
            />
            <button
              type="submit"
              aria-label="Save the column name"
              className="shrink-0 rounded p-1 text-emerald-600 hover:bg-emerald-50 dark:hover:bg-emerald-950"
            >
              <Check className="size-3.5" />
            </button>
            <button
              type="button"
              onClick={() => setRenaming(null)}
              aria-label="Keep the current name"
              className="shrink-0 rounded p-1 text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700"
            >
              <X className="size-3.5" />
            </button>
          </form>
        )}
      </ColumnHeader>

      <ColumnWell innerRef={setNodeRef} empty={cloneIds.length === 0 ? empty : null}>
        <SortableContext items={cloneIds} strategy={verticalListSortingStrategy}>
          {children}
        </SortableContext>
      </ColumnWell>
    </ColumnShell>
  );
}
