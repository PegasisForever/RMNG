// Imported accounts as a drag-reorderable, removable list. Each row is one email plus a grip
// and a trash button. One component, rendered once per provider, because the two sections are
// the same list; only the Claude one carries the import button, which is why that is a
// callback rather than a fixture of the markup.
//
// The order is a purely cosmetic client-side preference (localStorage) — the pool is
// unordered as far as the server is concerned, so it is NEVER sent with the config patch.
// The rail's usage panel reads the same store, so a reorder here shows up there live. The
// store itself belongs to the container: this list is handed rows already in order and
// reports the new one.
//
// Deleting an account removes its stored token, so it is confirmed. The container asks, for
// the same reason it owns the store: a confirm is a dialog, and a dialog is not markup this
// component can be a function of.
import {
  closestCenter,
  DndContext,
  type DragEndEvent,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  arrayMove,
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { GripVertical, Plus, Trash2 } from "lucide-react";

import type { ClaudeUsage } from "~/lib/types";

/** The drag sensors used by the account lists. `distance: 5` keeps a plain click on the grip
 *  from starting a drag, and the keyboard sensor makes the reorder reachable without a
 *  pointer (Space/Enter to grab, arrows to move) — matching the sidebar's clone list. */
function useReorderSensors() {
  return useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
}

/** One draggable imported-account row. The `GripVertical` handle is the ONLY element carrying
 *  the sortable listeners, so the delete button stays clickable. */
function SortableAccountRow({
  account,
  onDelete,
  onReplace,
}: {
  account: ClaudeUsage;
  onDelete: (email: string) => void;
  onReplace: (account: ClaudeUsage) => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: account.id,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    // `position: relative` so the raised z-index lifts the dragged row above its siblings.
    position: "relative",
    zIndex: isDragging ? 50 : undefined,
  };
  return (
    <li
      ref={setNodeRef}
      style={style}
      className={`flex items-center gap-2 rounded border border-slate-200 dark:border-slate-700 px-2.5 py-1.5 text-sm text-slate-700 dark:text-slate-200 ${
        isDragging ? "bg-white shadow-md ring-1 ring-slate-300 dark:bg-slate-800 dark:ring-slate-600" : ""
      }`}
    >
      <button
        type="button"
        {...attributes}
        {...listeners}
        aria-label={`reorder ${account.email}`}
        className="shrink-0 cursor-grab touch-none rounded p-0.5 text-slate-300 hover:text-slate-500 active:cursor-grabbing dark:text-slate-600 dark:hover:text-slate-400"
      >
        <GripVertical className="h-3.5 w-3.5" />
      </button>
      <span className="min-w-0 flex-1 truncate">{account.email}</span>
      {/* The same badge the rail's usage panel draws, and the same button. This list is where
          the repair used to happen by hand — delete the dead account, add its replacement,
          then put the pools and pins back — so it is the list that most needs the one-click
          version. */}
      {account.assignable === false ? (
        <button
          type="button"
          onClick={() => onReplace(account)}
          title={`${account.error ?? "the stored token expired and could not be refreshed"}\n\nSign in to replace this account.`}
          className="shrink-0 rounded bg-rose-100 px-1.5 py-0.5 text-[10px] font-medium text-rose-600 hover:bg-rose-200 dark:bg-rose-950/60 dark:text-rose-400 dark:hover:bg-rose-900/60"
        >
          sign in again
        </button>
      ) : null}
      <button
        type="button"
        title="delete account"
        aria-label={`delete ${account.email}`}
        onClick={() => onDelete(account.email)}
        className="shrink-0 rounded p-1 text-slate-400 hover:bg-red-50 hover:text-red-600 dark:text-slate-500 dark:hover:bg-red-950/40 dark:hover:text-red-400"
      >
        <Trash2 className="h-4 w-4" />
      </button>
    </li>
  );
}

export function SettingsAccountList({
  accounts,
  onDelete,
  onReplace,
  onReorder,
  onImport,
}: {
  /** This provider's rows, already in the operator's saved order. */
  accounts: ClaudeUsage[];
  onDelete: (email: string) => void;
  /** Sign in to an account that takes over from a dead one, from its "sign in again" badge. */
  onReplace: (account: ClaudeUsage) => void;
  onReorder: (orderedIds: string[]) => void;
  /** Import an account from a clone that is already signed in. Only the Claude section
   *  offers it: importing is provider-picked inside the same modal, so a second entry point
   *  under Codex would open the same dialog. */
  onImport?: () => void;
}) {
  const sensors = useReorderSensors();
  const ids = accounts.map((a) => a.id);

  function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = ids.indexOf(String(active.id));
    const newIndex = ids.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    onReorder(arrayMove(ids, oldIndex, newIndex));
  }

  const list =
    accounts.length === 0 ? (
      <p className="text-xs text-slate-400 dark:text-slate-500">
        None imported yet — import one from a clone that's already signed in.
      </p>
    ) : (
      <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
        <SortableContext items={ids} strategy={verticalListSortingStrategy}>
          <ul className="space-y-1.5">
            {accounts.map((a) => (
              <SortableAccountRow
                key={a.id}
                account={a}
                onDelete={onDelete}
                onReplace={onReplace}
              />
            ))}
          </ul>
        </SortableContext>
      </DndContext>
    );

  if (!onImport) return list;
  return (
    <div className="space-y-2">
      {list}
      <button
        type="button"
        onClick={onImport}
        className="inline-flex items-center gap-1 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
      >
        <Plus className="size-3.5" /> Import account
      </button>
    </div>
  );
}
