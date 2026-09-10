// The single account-pool list: groups of mixed Claude/Codex members as a drag tree.
// Groups reorder vertically; members reorder inside their group and move across groups
// (a cross-group drag MOVES — nothing is left behind; the per-group "clone" picker copies
// instead). One account in several groups is allowed; two of the same in one group is
// refused (the drop is a no-op).
//
// Everything here is a draft edit (`onChange`) — including dropping a membership. An
// account that ends up in no group is deleted by the save (`PUT /api/config` sweeps
// unclaimed accounts), so the tree warns about those live instead of deleting anything
// itself. Importing is immediate (a token lands now, not on save), so the per-group
// import buttons stay callbacks the container owns.
import {
  closestCenter,
  DndContext,
  type DragEndEvent,
  type DragOverEvent,
  KeyboardSensor,
  PointerSensor,
  useDroppable,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { GripVertical, X } from "lucide-react";
import { useRef, useState } from "react";

import { settingsInput } from "~/components/SettingsFields";
import {
  addReference,
  moveMember,
  reorderGroups,
  type TreeGroup,
} from "~/lib/groupTree";
import { newGroup, type GroupDraft } from "~/lib/settingsDraft";
import type { ClaudeUsage } from "~/lib/types";

/** dnd ids. Group order can change mid-drag only for the active group drag, and members
 *  only move while one is active, so index-based ids stay stable for the drag's lifetime. */
const groupId = (gi: number) => `g:${gi}`;
const memberId = (gi: number, email: string) => `m:${gi}:${email}`;
const emptyId = (gi: number) => `empty:${gi}`;

function parseId(id: string): { kind: "group" | "member" | "empty"; gi: number; email?: string } | null {
  const [kind, gi, ...rest] = id.split(":");
  if (kind !== "g" && kind !== "m" && kind !== "empty") return null;
  const giNum = Number(gi);
  if (!Number.isInteger(giNum)) return null;
  return {
    kind: kind === "g" ? "group" : kind === "m" ? "member" : "empty",
    gi: giNum,
    email: rest.join(":") || undefined,
  };
}

function providerOf(accounts: ClaudeUsage[], email: string): string {
  // A draft can still name an account deleted elsewhere; "unknown" says so honestly.
  return accounts.find((a) => a.email === email)?.provider ?? "unknown";
}

export function SettingsGroupsEditor({
  groups,
  accounts,
  noAccountsHint,
  onChange,
  onImportAccount,
}: {
  groups: GroupDraft[];
  /** Every imported account, both providers — provider chips, the clone picker source,
   *  and the ungrouped warning below. */
  accounts: ClaudeUsage[];
  /** What the tree says when there is nothing to put in it (no accounts imported at all). */
  noAccountsHint: string;
  onChange: (groups: GroupDraft[]) => void;
  /** Per-group import buttons: importing lands the account in that group now. */
  onImportAccount: (provider: "claude" | "codex", group: string) => void;
}) {
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  // The tree mid-drag. Props stay the save source; this copy renders while a drag is
  // active and is the base each drag-over computes from (a state updater would double-apply
  // under StrictMode, so the ref — never the updater — is the arithmetic source).
  const [dragging, setDragging] = useState<TreeGroup[] | null>(null);
  const dragRef = useRef<TreeGroup[] | null>(null);
  const startRef = useRef<TreeGroup[] | null>(null);
  const shown = dragging ?? groups;

  const memberships = (email: string) =>
    shown.filter((g) => g.accounts.includes(email)).length;

  const rename = (gi: number, name: string) =>
    onChange(shown.map((g, j) => (j === gi ? { ...g, name } : g)));

  const dropMember = (gi: number, email: string) => {
    const base = dragRef.current ?? groups;
    const next = base.map((g, j) =>
      j === gi ? { ...g, accounts: g.accounts.filter((e) => e !== email) } : g,
    );
    dragRef.current = dragging ? next : null;
    if (dragging) setDragging(next);
    onChange(next);
  };

  const onDragStart = () => {
    startRef.current = groups;
    dragRef.current = groups;
    setDragging(groups);
  };

  const onDragOver = (e: DragOverEvent) => {
    const { active, over } = e;
    if (!over) return;
    const a = parseId(String(active.id));
    const o = parseId(String(over.id));
    if (!a || !o) return;
    const base = dragRef.current ?? groups;
    if (a.kind === "group") {
      if (o.kind !== "group" || a.gi === o.gi) return;
      applyNext(reorderGroups(base, a.gi, o.gi));
      return;
    }
    // A member drag: the target group is the over item's group (or the empty slot's,
    // or the group header's). Insert before the over member, append otherwise.
    const fromGi = a.gi;
    const fromIdx = base[fromGi]?.accounts.indexOf(a.email ?? "") ?? -1;
    if (fromIdx < 0) return;
    const toGi = o.gi;
    const toIdx =
      o.kind === "member"
        ? (base[toGi]?.accounts.indexOf(o.email ?? "") ?? 0)
        : (base[toGi]?.accounts.length ?? 0);
    if (fromGi === toGi && (toIdx === fromIdx || toIdx === fromIdx + 1)) return;
    const next = moveMember(base, { group: fromGi, index: fromIdx }, { group: toGi, index: toIdx });
    if (next) applyNext(next);
    // A refused drop (duplicate) leaves the tree untouched.
  };

  const applyNext = (next: TreeGroup[]) => {
    dragRef.current = next;
    setDragging(next);
    onChange(next);
  };

  const endDrag = (revert: boolean) => {
    // Dropped outside any target: put back what the drag started from.
    if (revert && startRef.current) onChange(startRef.current);
    startRef.current = null;
    dragRef.current = null;
    setDragging(null);
  };

  const onDragEnd = (e: DragEndEvent) => endDrag(!e.over);

  // Imported accounts claimed by no group: saving deletes them (the server sweeps
  // unclaimed accounts on a groups-touching save), so they are listed, not hidden.
  const claimed = new Set(shown.flatMap((g) => g.accounts));
  const ungrouped = accounts.filter((a) => !claimed.has(a.email));

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={closestCenter}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
      onDragCancel={() => endDrag(true)}
    >
      <div className="space-y-3">
        {shown.length === 0 ? <p className="text-xs text-slate-400 dark:text-slate-500">No groups.</p> : null}
        <SortableContext items={shown.map((_, gi) => groupId(gi))} strategy={verticalListSortingStrategy}>
          {shown.map((g, gi) => (
            <GroupCard
              key={groupId(gi)}
              gi={gi}
              group={g}
              accounts={accounts}
              memberships={memberships}
              onRename={(name) => rename(gi, name)}
              onRemoveGroup={() => onChange(shown.filter((_, j) => j !== gi))}
              onDropMember={(email) => dropMember(gi, email)}
              onReference={(email) => {
                const next = addReference(dragging ?? groups, gi, email);
                if (next) onChange(next);
              }}
              onImportAccount={(provider) => onImportAccount(provider, g.name)}
            />
          ))}
        </SortableContext>
        {accounts.length === 0 ? (
          <p className="text-xs text-slate-400 dark:text-slate-500">{noAccountsHint}</p>
        ) : null}
        {ungrouped.length > 0 ? (
          <p className="text-xs text-amber-600 dark:text-amber-400">
            Not in any group — saving removes {ungrouped.length === 1 ? "it" : "them"} entirely:{" "}
            {ungrouped.map((a) => a.email).join(", ")}. Reference {ungrouped.length === 1 ? "it" : "them"} into a
            group above to keep {ungrouped.length === 1 ? "it" : "them"}.
          </p>
        ) : null}
        <button
          type="button"
          onClick={() => onChange([...groups, newGroup()])}
          className="rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          + Create group
        </button>
      </div>
    </DndContext>
  );
}

function GroupCard({
  gi,
  group,
  accounts,
  memberships,
  onRename,
  onRemoveGroup,
  onDropMember,
  onReference,
  onImportAccount,
}: {
  gi: number;
  group: GroupDraft;
  accounts: ClaudeUsage[];
  memberships: (email: string) => number;
  onRename: (name: string) => void;
  onRemoveGroup: () => void;
  onDropMember: (email: string) => void;
  onReference: (email: string) => void;
  onImportAccount: (provider: "claude" | "codex") => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: groupId(gi),
  });
  const { setNodeRef: setEmptyRef, isOver: emptyOver } = useDroppable({ id: emptyId(gi) });
  // Accounts this group does not hold yet: members of other groups plus ungrouped ones.
  // The button copies (references) rather than moves — dragging is what moves.
  const referenceable = accounts.filter((a) => !group.accounts.includes(a.email));
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    position: "relative",
    zIndex: isDragging ? 50 : undefined,
  };
  return (
    <div
      ref={setNodeRef}
      style={style}
      className={`rounded border border-slate-200 dark:border-slate-700 p-3 ${isDragging ? "bg-white shadow-md ring-1 ring-slate-300 dark:bg-slate-800 dark:ring-slate-600" : ""}`}
    >
      <div className="flex items-center gap-2">
        <button
          type="button"
          {...attributes}
          {...listeners}
          aria-label={`reorder group ${group.name || gi + 1}`}
          className="shrink-0 cursor-grab touch-none rounded p-0.5 text-slate-300 hover:text-slate-500 active:cursor-grabbing dark:text-slate-600 dark:hover:text-slate-400"
        >
          <GripVertical size={14} />
        </button>
        <input
          value={group.name}
          onChange={(e) => onRename(e.target.value)}
          placeholder="group name"
          className={`${settingsInput} flex-1`}
        />
        <button
          type="button"
          onClick={onRemoveGroup}
          className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800"
        >
          Remove
        </button>
      </div>
      <SortableContext items={group.accounts.map((email) => memberId(gi, email))} strategy={verticalListSortingStrategy}>
        <ul className="mt-2 space-y-1">
          {group.accounts.map((email) => (
            <MemberRow
              key={memberId(gi, email)}
              id={memberId(gi, email)}
              email={email}
              provider={providerOf(accounts, email)}
              lastGroup={memberships(email) <= 1}
              onDrop={() => onDropMember(email)}
            />
          ))}
        </ul>
      </SortableContext>
      {group.accounts.length === 0 ? (
        <div
          ref={setEmptyRef}
          className={`mt-2 rounded border border-dashed px-2 py-3 text-center text-xs ${emptyOver ? "border-emerald-500 text-emerald-600 dark:text-emerald-400" : "border-slate-200 text-slate-400 dark:border-slate-700 dark:text-slate-500"}`}
        >
          Drag accounts here
        </div>
      ) : null}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={() => onImportAccount("claude")}
          className="rounded border border-slate-300 dark:border-slate-600 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          + Import claude
        </button>
        <button
          type="button"
          onClick={() => onImportAccount("codex")}
          className="rounded border border-slate-300 dark:border-slate-600 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          + Import codex
        </button>
        {referenceable.length > 0 ? (
          <select
            value=""
            onChange={(e) => {
              if (e.target.value) onReference(e.target.value);
              e.target.value = "";
            }}
            className="rounded border border-slate-300 dark:border-slate-600 px-1 py-0.5 text-xs text-slate-600 dark:text-slate-300 dark:bg-slate-800"
            aria-label={`reference an existing account into ${group.name || "this group"}`}
          >
            <option value="">+ Clone an existing account…</option>
            {referenceable.map((a) => (
              <option key={a.id} value={a.email}>
                {a.email}
              </option>
            ))}
          </select>
        ) : null}
      </div>
    </div>
  );
}

function MemberRow({
  id,
  email,
  provider,
  lastGroup,
  onDrop,
}: {
  id: string;
  email: string;
  provider: string;
  /** This is the account's only group: dropping it routes to account delete on save. */
  lastGroup: boolean;
  onDrop: () => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    position: "relative",
    zIndex: isDragging ? 50 : undefined,
  };
  return (
    <li
      ref={setNodeRef}
      style={style}
      className={`flex items-center gap-2 rounded border border-slate-200 dark:border-slate-700 px-2 py-1 text-xs text-slate-700 dark:text-slate-200 ${isDragging ? "bg-white shadow-md ring-1 ring-slate-300 dark:bg-slate-800 dark:ring-slate-600" : ""}`}
    >
      <button
        type="button"
        {...attributes}
        {...listeners}
        aria-label={`reorder ${email}`}
        className="shrink-0 cursor-grab touch-none rounded p-0.5 text-slate-300 hover:text-slate-500 active:cursor-grabbing dark:text-slate-600 dark:hover:text-slate-400"
      >
        <GripVertical size={12} />
      </button>
      <span className="shrink-0 rounded bg-slate-100 dark:bg-slate-700 px-1.5 py-px text-[10px] uppercase tracking-wide text-slate-500 dark:text-slate-400">
        {provider}
      </span>
      <span className="min-w-0 flex-1 truncate">{email}</span>
      <button
        type="button"
        onClick={onDrop}
        title={
          lastGroup
            ? "Remove from its last group — saving deletes this account entirely"
            : "Remove from this group (it stays in its other groups)"
        }
        aria-label={`remove ${email} from this group`}
        className="shrink-0 rounded p-0.5 text-slate-400 hover:bg-slate-100 hover:text-slate-600 dark:text-slate-500 dark:hover:bg-slate-800 dark:hover:text-slate-300"
      >
        <X size={12} />
      </button>
    </li>
  );
}
