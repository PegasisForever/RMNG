// The board's column list, as edited in Settings: add, rename, reorder, delete. The board
// itself only moves clones between the columns it is given, so every structural edit is here.
//
// Reordering is a drag on the grip alone, not the whole row: the row also holds a text
// input, and a row-wide drag activator would eat the clicks that put a caret in it.
import {
  closestCenter,
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
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
import { type CSSProperties, useState } from "react";

import { BUTTON, CARD_BOX, FIELD, ICON_BUTTON, TEXT, TEXT_2, TEXT_3 } from "~/desk/control/ui";
import type { BoardColumn } from "~/lib/board";

export interface BoardColumnsEditorProps {
  columns: BoardColumn[];
  /** How many clones each column holds right now, so a delete can say what it displaces.
   *  Keyed by column id; a missing entry counts as zero. */
  counts?: Record<string, number>;
  onAddColumn: (title: string) => void;
  onRenameColumn: (columnId: string, title: string) => void;
  /** Turn the drop-here-to-archive rule on or off for one column. */
  onSetArchive: (columnId: string, archive: boolean) => void;
  onDeleteColumn: (columnId: string) => void;
  /** The new column order, left to right on the board. */
  onReorderColumns: (columnIds: string[]) => void;
}

function ColumnRow({
  column,
  count,
  onRename,
  onSetArchive,
  onDelete,
}: {
  column: BoardColumn;
  count: number;
  onRename: (title: string) => void;
  onSetArchive: (archive: boolean) => void;
  onDelete: () => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: column.id,
  });
  const style: CSSProperties = {
    transform: CSS.Translate.toString(transform),
    transition,
    zIndex: isDragging ? 1 : undefined,
  };

  return (
    <li
      ref={setNodeRef}
      style={style}
      // A card; lifted while it is dragged
      className={`flex items-center gap-2 px-2 py-1.5 ${CARD_BOX} ${isDragging ? "shadow-lg" : ""}`}
    >
      <button
        type="button"
        {...attributes}
        {...listeners}
        aria-label={`Reorder ${column.title}`}
        data-tip="Drag to reorder"
        className={`shrink-0 cursor-grab touch-none p-1 active:cursor-grabbing ${ICON_BUTTON}`}
      >
        <GripVertical className="size-4" />
      </button>
      <input
        value={column.title}
        onChange={(e) => onRename(e.target.value)}
        aria-label={`${column.title} name`}
        // A field only under the mouse or while typed in: in a row of them, ten boxes are noise
        className={`min-w-0 flex-1 rounded-md border border-transparent bg-transparent px-2 py-1 text-sm outline-none hover:border-black/10 focus:border-black/30 dark:hover:border-white/10 dark:focus:border-white/30 ${TEXT}`}
      />
      <label
        className={`flex shrink-0 cursor-pointer items-center gap-1 text-xs ${TEXT_2}`}
        data-tip="Archive a clone when it is dropped in this column, and start it again when it is dragged out"
      >
        <input
          type="checkbox"
          checked={column.archive}
          onChange={(e) => onSetArchive(e.target.checked)}
          className="size-3.5 accent-emerald-600"
        />
        archive
      </label>
      <span className={`shrink-0 rounded-full bg-black/5 px-2 text-xs font-medium tabular-nums dark:bg-white/10 ${TEXT_2}`}>
        {count}
      </span>
      <button
        type="button"
        onClick={onDelete}
        data-tip={
          count > 0
            ? `Delete this column; its ${count} clone${count === 1 ? "" : "s"} move to the first one`
            : "Delete this column"
        }
        aria-label={`Delete the ${column.title} column`}
        className={`shrink-0 p-1 ${ICON_BUTTON} hover:text-rose-600 dark:hover:text-rose-400`}
      >
        <Trash2 className="size-4" />
      </button>
    </li>
  );
}

export function BoardColumnsEditor({
  columns,
  counts = {},
  onAddColumn,
  onRenameColumn,
  onSetArchive,
  onDeleteColumn,
  onReorderColumns,
}: BoardColumnsEditorProps) {
  const [title, setTitle] = useState("");

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const ids = columns.map((column) => column.id);

  const onDragEnd = ({ active, over }: DragEndEvent) => {
    if (!over || active.id === over.id) return;
    const from = ids.indexOf(String(active.id));
    const to = ids.indexOf(String(over.id));
    if (from < 0 || to < 0) return;
    onReorderColumns(arrayMove(ids, from, to));
  };

  const add = () => {
    const next = title.trim();
    if (!next) return;
    onAddColumn(next);
    setTitle("");
  };

  return (
    <div className="space-y-3">
      {columns.length === 0 ? (
        <p className={`rounded-lg border border-dashed border-black/15 p-4 text-center text-xs dark:border-white/15 ${TEXT_3}`}>
          No columns yet. Add one below.
        </p>
      ) : (
        <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
          <SortableContext items={ids} strategy={verticalListSortingStrategy}>
            <ul className="space-y-2">
              {columns.map((column) => (
                <ColumnRow
                  key={column.id}
                  column={column}
                  count={counts[column.id] ?? 0}
                  onRename={(next) => onRenameColumn(column.id, next)}
                  onSetArchive={(archive) => onSetArchive(column.id, archive)}
                  onDelete={() => onDeleteColumn(column.id)}
                />
              ))}
            </ul>
          </SortableContext>
        </DndContext>
      )}

      <div className="flex gap-2">
        <input
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
          placeholder="New column name"
          aria-label="New column name"
          className={`min-w-0 flex-1 px-2 py-1 text-sm placeholder:text-black/40 dark:placeholder:text-white/40 ${FIELD}`}
        />
        <button
          type="button"
          onClick={add}
          disabled={!title.trim()}
          className={`flex shrink-0 items-center gap-1 px-2 py-1 text-xs disabled:opacity-40 ${BUTTON}`}
        >
          <Plus className="size-3.5" />
          Add column
        </button>
      </div>
    </div>
  );
}
