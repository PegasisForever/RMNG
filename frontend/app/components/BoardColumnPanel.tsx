// One board column: a header (title, card count, and for operator-made columns a rename
// and a delete) over a droppable, scrollable card list.
//
// The list stays a drop target when it is empty, which is what `useDroppable` on the body
// is for: with only the cards registered, an empty column could never receive the first one.
import { useDroppable } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { Check, Plus, X } from "lucide-react";
import { type ReactNode, useState } from "react";

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
    <section className="flex w-80 shrink-0 flex-col rounded-xl bg-slate-100 dark:bg-slate-800">
      <header className="flex shrink-0 items-center gap-2 px-3 pt-3 pb-2">
        {renaming === null ? (
          <>
            <h2
              onDoubleClick={() => setRenaming(title)}
              title="Double-click to rename"
              className="min-w-0 flex-1 truncate text-sm font-semibold text-slate-700 dark:text-slate-200"
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
      </header>

      {/* No hover tint on the drop target. The cards themselves already part around the
          pointer to show where the drop lands, so a colour wash on the column only repeats
          it, and it repeats it a whole column wide. */}
      <div
        ref={setNodeRef}
        className="min-h-0 flex-1 space-y-2 overflow-y-auto rounded-b-xl px-2 pb-2"
      >
        <SortableContext items={cloneIds} strategy={verticalListSortingStrategy}>
          {children}
        </SortableContext>
        {cloneIds.length === 0 ? (
          <p className="rounded-lg border border-dashed border-slate-300 p-4 text-center text-xs text-slate-400 dark:border-slate-600 dark:text-slate-500">
            {empty}
          </p>
        ) : null}
      </div>
    </section>
  );
}
