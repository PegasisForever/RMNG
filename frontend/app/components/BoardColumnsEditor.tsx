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

import type { BoardColumn } from "~/lib/board";

export interface BoardColumnsEditorProps {
  columns: BoardColumn[];
  /** How many clones each column holds right now, so a delete can say what it displaces.
   *  Keyed by column id; a missing entry counts as zero. */
  counts?: Record<string, number>;
  onAddColumn: (title: string) => void;
  onRenameColumn: (columnId: string, title: string) => void;
  onDeleteColumn: (columnId: string) => void;
  /** The new column order, left to right on the board. */
  onReorderColumns: (columnIds: string[]) => void;
}

function ColumnRow({
  column,
  count,
  onRename,
  onDelete,
}: {
  column: BoardColumn;
  count: number;
  onRename: (title: string) => void;
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
      className={`flex items-center gap-2 rounded-lg border bg-white px-2 py-1.5 dark:bg-slate-800 ${
        isDragging
          ? "border-slate-300 shadow-lg dark:border-slate-500"
          : "border-slate-200 dark:border-slate-700"
      }`}
    >
      <button
        type="button"
        {...attributes}
        {...listeners}
        aria-label={`Reorder ${column.title}`}
        title="Drag to reorder"
        className="shrink-0 cursor-grab touch-none rounded p-1 text-slate-400 hover:bg-slate-100 active:cursor-grabbing dark:text-slate-500 dark:hover:bg-slate-700"
      >
        <GripVertical className="size-4" />
      </button>
      <input
        value={column.title}
        onChange={(e) => onRename(e.target.value)}
        aria-label={`${column.title} name`}
        className="min-w-0 flex-1 rounded-md border border-transparent bg-transparent px-2 py-1 text-sm text-slate-900 hover:border-slate-300 focus:border-emerald-500 focus:outline-none dark:text-slate-100 dark:hover:border-slate-600"
      />
      <span className="shrink-0 rounded-full bg-slate-100 px-2 text-xs font-medium tabular-nums text-slate-500 dark:bg-slate-700 dark:text-slate-400">
        {count}
      </span>
      <button
        type="button"
        onClick={onDelete}
        title={
          count > 0
            ? `Delete this column; its ${count} clone${count === 1 ? "" : "s"} move to the first one`
            : "Delete this column"
        }
        aria-label={`Delete the ${column.title} column`}
        className="shrink-0 rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-red-600 dark:text-slate-500 dark:hover:bg-slate-700 dark:hover:text-red-400"
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
        <p className="rounded-lg border border-dashed border-slate-300 p-4 text-center text-xs text-slate-400 dark:border-slate-600 dark:text-slate-500">
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
          className="min-w-0 flex-1 rounded border border-slate-300 px-2 py-1 text-sm text-slate-900 placeholder:text-slate-400 focus:border-slate-400 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500 dark:focus:border-slate-500"
        />
        <button
          type="button"
          onClick={add}
          disabled={!title.trim()}
          className="flex shrink-0 items-center gap-1 rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50 disabled:opacity-40 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-800"
        >
          <Plus className="size-3.5" />
          Add column
        </button>
      </div>
    </div>
  );
}

export default BoardColumnsEditor;
