// A two-level drag tree. Hover only selects an insertion boundary; it never edits
// the draft. The source stays mounted, and a separate overlay follows the pointer.
import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  MeasuringStrategy,
  PointerSensor,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import { GripVertical, X } from "lucide-react";
import { Fragment, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";

import chatgptLogo from "~/assets/chatgpt.svg";
import claudeLogo from "~/assets/claude.svg";
import { settingsInput } from "~/components/SettingsFields";
import {
  applyTreeDrop,
  isDuplicateDrop,
  type EditorGroup,
  type TreeDragItem,
  type TreeDropTarget,
} from "~/lib/groupTreeDrag";
import { newGroup, type GroupDraft } from "~/lib/settingsDraft";
import type { ClaudeUsage } from "~/lib/types";
import {
  itemId,
  targetId,
  treeCollision,
  treeKeyboardCoordinates,
} from "./SettingsGroupsEditor.dnd";

const actionClass =
  "rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50 disabled:opacity-40 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-800";
const gripClass =
  "shrink-0 cursor-grab touch-none rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600 focus-visible:outline-2 focus-visible:outline-blue-500 active:cursor-grabbing dark:hover:bg-slate-700 dark:hover:text-slate-200";

function providerOf(accounts: ClaudeUsage[], email: string): string {
  return (
    accounts.find((account) => account.email === email)?.provider ?? "unknown"
  );
}

export function SettingsGroupsEditor({
  groups,
  accounts,
  noAccountsHint,
  onChange,
  onImportAccount,
}: {
  groups: GroupDraft[];
  /** Both providers, including imported accounts not yet in a group. */
  accounts: ClaudeUsage[];
  noAccountsHint: string;
  onChange: (groups: GroupDraft[]) => void;
  /** Import writes the token immediately; all other changes stay in the draft. */
  onImportAccount: (provider: "claude" | "codex", group: string) => void;
}) {
  const prefix = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const nextGroupId = useRef(0);
  const dragSource = useRef<GroupDraft[] | null>(null);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(KeyboardSensor, {
      coordinateGetter: treeKeyboardCoordinates,
      scrollBehavior: "auto",
    }),
  );
  const [editor, setEditor] = useState(() => ({
    source: groups,
    nodes: groups.map((group, index) => ({
      ...group,
      id: `${prefix}:0:${index}`,
    })),
    revision: 0,
  }));
  const [active, setActive] = useState<TreeDragItem | null>(null);
  const [target, setTarget] = useState<TreeDropTarget | null>(null);

  // Preserve identities across our edits, including rename and reorder. An external
  // replacement (save, reset, import) starts a new tree and cancels any old sensor.
  // IDs are editor-only and are never sent through the settings interface.
  if (editor.source !== groups) {
    dragSource.current = null;
    const revision = editor.revision + 1;
    setEditor({
      source: groups,
      nodes: groups.map((group, index) => ({
        ...group,
        id: `${prefix}:${revision}:${index}`,
      })),
      revision,
    });
    setActive(null);
    setTarget(null);
  }
  const nodes = editor.nodes;
  const publish = (next: EditorGroup[]) => {
    const value = next.map(({ name, accounts: members }) => ({
      name,
      accounts: members,
    }));
    setEditor({ ...editor, source: value, nodes: next });
    onChange(value);
  };
  const updateGroup = (
    id: string,
    update: (group: EditorGroup) => EditorGroup,
  ) => publish(nodes.map((group) => (group.id === id ? update(group) : group)));
  const clearDrag = () => {
    dragSource.current = null;
    setActive(null);
    setTarget(null);
  };
  const onDragEnd = (event: DragEndEvent) => {
    // A sensor may deliver release after a reset or import replaced its provider.
    // Never let that old session overwrite the new draft.
    if (dragSource.current !== groups) return;
    const item = event.active.data.current?.item as TreeDragItem | undefined;
    const destination = event.over?.data.current?.target as
      | TreeDropTarget
      | undefined;
    if (item && destination) {
      const next = applyTreeDrop(nodes, item, destination);
      if (next) {
        publish(next);
        // A cross-group move mounts a new row. dnd-kit's default restoration only
        // knows the old handle, so explicitly follow the moved membership.
        if (event.activatorEvent instanceof KeyboardEvent) {
          const moved =
            item.kind === "member" && destination.kind === "member"
              ? { ...item, groupId: destination.groupId }
              : item;
          requestAnimationFrame(() =>
            rootRef.current
              ?.querySelector<HTMLButtonElement>(
                `[data-drag-handle="${CSS.escape(itemId(moved))}"]`,
              )
              ?.focus(),
          );
        }
      }
    }
    clearDrag();
  };
  const duplicate =
    !!active && !!target && isDuplicateDrop(nodes, active, target);
  const claimed = new Set(nodes.flatMap((group) => group.accounts));
  const ungrouped = accounts.filter((account) => !claimed.has(account.email));
  const sourceGroup = nodes.find((group) => group.id === active?.groupId);
  const destinationGroup =
    target?.kind === "member"
      ? nodes.find((group) => group.id === target.groupId)
      : null;
  const status = active
    ? duplicate
      ? "This group already contains this account. Release to keep it in its original group."
      : target
        ? target.kind === "group"
          ? `Insert group at position ${target.index + 1}.`
          : `Move to ${destinationGroup?.name || "unnamed group"}, position ${target.index + 1}.`
        : "Move over a group to choose a position. Release outside or press Escape to cancel."
    : "Drag a handle to move. Use Space and arrow keys with the keyboard.";

  return (
    <DndContext
      key={editor.revision}
      sensors={sensors}
      collisionDetection={treeCollision}
      measuring={{ droppable: { strategy: MeasuringStrategy.Always } }}
      accessibility={{
        screenReaderInstructions: {
          draggable:
            "Press Space to pick up. Use Up and Down to choose a position, including other groups. Press Space to drop or Escape to cancel.",
        },
        announcements: {
          onDragStart: () => "Picked up. Use Up and Down to choose a position.",
          onDragOver: () => status,
          onDragEnd: () => "Drag ended.",
          onDragCancel: () => "Move cancelled. The groups have not changed.",
        },
      }}
      onDragStart={({ active: drag }) => {
        dragSource.current = groups;
        setActive(drag.data.current?.item as TreeDragItem);
        setTarget(null);
      }}
      onDragOver={({ over }) => {
        if (dragSource.current === groups)
          setTarget(
            (over?.data.current?.target as TreeDropTarget | undefined) ?? null,
          );
      }}
      onDragEnd={onDragEnd}
      onDragCancel={() => {
        if (dragSource.current === groups) clearDrag();
      }}
    >
      <div ref={rootRef} data-groups-editor>
        <p role="status" className="sr-only">
          {status}
        </p>
        <TreeRegion>
          <DropSlot
            target={{ kind: "group", index: 0 }}
            activeKind={active?.kind}
          />
          {nodes.map((group, index) => (
            <Fragment key={group.id}>
              <GroupCard
                group={group}
                index={index}
                accounts={accounts}
                active={active}
                duplicate={duplicate}
                memberships={(email) =>
                  nodes.filter((node) => node.accounts.includes(email)).length
                }
                onRename={(name) =>
                  updateGroup(group.id, (node) => ({ ...node, name }))
                }
                onRemove={() =>
                  publish(nodes.filter((node) => node.id !== group.id))
                }
                onRemoveMember={(email) =>
                  updateGroup(group.id, (node) => ({
                    ...node,
                    accounts: node.accounts.filter(
                      (member) => member !== email,
                    ),
                  }))
                }
                onReference={(email) => {
                  if (!group.accounts.includes(email))
                    updateGroup(group.id, (node) => ({
                      ...node,
                      accounts: [...node.accounts, email],
                    }));
                }}
                onImport={(provider) => onImportAccount(provider, group.name)}
              />
              <DropSlot
                target={{ kind: "group", index: index + 1 }}
                activeKind={active?.kind}
              />
            </Fragment>
          ))}
          {nodes.length === 0 && (
            <p className="py-3 text-xs text-slate-400">No groups.</p>
          )}
        </TreeRegion>
        {accounts.length === 0 && (
          <p className="my-2 text-xs text-slate-400">{noAccountsHint}</p>
        )}
        {ungrouped.length > 0 && (
          <p className="my-2 text-xs text-amber-600 dark:text-amber-400">
            Not in any group — saving deletes these accounts:{" "}
            {ungrouped.map((account) => account.email).join(", ")}. Add them to
            a group to keep them.
          </p>
        )}
        <button
          type="button"
          disabled={!!active}
          onClick={() =>
            publish([
              ...nodes,
              { ...newGroup(), id: `${prefix}:added:${nextGroupId.current++}` },
            ])
          }
          className={`mt-2 ${actionClass}`}
        >
          + Create group
        </button>
      </div>
      {typeof document !== "undefined" &&
        createPortal(
          <DragOverlay
            dropAnimation={null}
            className="pointer-events-none"
            zIndex={10000}
          >
            {active && sourceGroup ? (
              <div
                data-drag-preview
                className="rounded border border-blue-400 bg-white p-3 text-sm text-slate-800 shadow-xl dark:bg-slate-800 dark:text-slate-100"
              >
                <div className="flex items-center gap-2">
                  <GripVertical size={16} />
                  {active.kind === "member" ? (
                    <AccountLabel
                      email={active.email}
                      provider={providerOf(accounts, active.email)}
                    />
                  ) : (
                    <strong>{sourceGroup.name || "Unnamed group"}</strong>
                  )}
                </div>
                {duplicate && (
                  <p className="mt-1 text-xs text-amber-700 dark:text-amber-400">
                    Already in this group
                  </p>
                )}
              </div>
            ) : null}
          </DragOverlay>,
          document.body,
        )}
    </DndContext>
  );
}

function TreeRegion({ children }: { children: React.ReactNode }) {
  const { setNodeRef } = useDroppable({
    id: "groups-region",
    data: { treeRegion: true },
  });
  return <div ref={setNodeRef}>{children}</div>;
}

/** Slots occupy fixed space even when inactive. Highlighting one never moves a row. */
function DropSlot({
  target,
  activeKind,
  blocked = false,
}: {
  target: TreeDropTarget;
  activeKind?: TreeDragItem["kind"];
  blocked?: boolean;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: targetId(target),
    data: { target },
  });
  const show = isOver && activeKind === target.kind;
  return (
    <div
      ref={setNodeRef}
      data-drop-kind={target.kind}
      data-drop-index={target.index}
      data-drop-active={show || undefined}
      className={`relative ${target.kind === "group" ? "h-3" : "h-2"}`}
    >
      {show && (
        <div
          className={`pointer-events-none absolute inset-x-0 top-1/2 border-t-2 ${blocked ? "border-dashed border-amber-500" : "border-blue-500"}`}
        />
      )}
    </div>
  );
}

function GroupCard({
  group,
  index,
  accounts,
  active,
  duplicate,
  memberships,
  onRename,
  onRemove,
  onRemoveMember,
  onReference,
  onImport,
}: {
  group: EditorGroup;
  index: number;
  accounts: ClaudeUsage[];
  active: TreeDragItem | null;
  duplicate: boolean;
  memberships: (email: string) => number;
  onRename: (name: string) => void;
  onRemove: () => void;
  onRemoveMember: (email: string) => void;
  onReference: (email: string) => void;
  onImport: (provider: "claude" | "codex") => void;
}) {
  const item: TreeDragItem = { kind: "group", groupId: group.id };
  const { attributes, listeners, setNodeRef, setActivatorNodeRef, isDragging } =
    useDraggable({ id: itemId(item), data: { item } });
  const { setNodeRef: setRegionRef } = useDroppable({
    id: `region:${group.id}`,
    data: { groupRegion: group.id },
  });
  const referenceable = accounts.filter(
    (account) => !group.accounts.includes(account.email),
  );
  return (
    <section
      ref={setRegionRef}
      data-group-name={group.name}
      data-group-id={group.id}
      aria-label={`Group ${group.name || index + 1}`}
    >
      <div
        ref={setNodeRef}
        className={`rounded border border-slate-200 bg-white dark:border-slate-700 dark:bg-slate-900 ${isDragging ? "opacity-40" : ""}`}
      >
        <div className="flex items-center gap-2 rounded-t bg-slate-50 p-2 dark:bg-slate-800/60">
          <button
            ref={setActivatorNodeRef}
            data-drag-handle={itemId(item)}
            type="button"
            {...attributes}
            {...listeners}
            aria-label={`reorder group ${group.name || index + 1}`}
            className={gripClass}
          >
            <GripVertical size={16} />
          </button>
          <input
            aria-label={`Group ${index + 1} name`}
            disabled={!!active}
            value={group.name}
            onChange={(event) => onRename(event.target.value)}
            placeholder="group name"
            className={`${settingsInput} min-w-0 flex-1`}
          />
          <button
            type="button"
            disabled={!!active}
            onClick={onRemove}
            aria-label={`Remove group ${group.name || index + 1}`}
            title="Remove group"
            className="shrink-0 rounded p-1 text-slate-400 hover:bg-slate-200 disabled:opacity-40 dark:hover:bg-slate-700"
          >
            <X size={14} />
          </button>
        </div>
        <div className="ml-10 mr-3">
          <DropSlot
            target={{ kind: "member", groupId: group.id, index: 0 }}
            activeKind={active?.kind}
            blocked={duplicate}
          />
          {group.accounts.map((email, memberIndex) => (
            <Fragment key={email}>
              <MemberRow
                item={{ kind: "member", groupId: group.id, email }}
                provider={providerOf(accounts, email)}
                disabled={!!active}
                lastGroup={memberships(email) <= 1}
                onRemove={() => onRemoveMember(email)}
              />
              <DropSlot
                target={{
                  kind: "member",
                  groupId: group.id,
                  index: memberIndex + 1,
                }}
                activeKind={active?.kind}
                blocked={duplicate}
              />
            </Fragment>
          ))}
          {group.accounts.length === 0 && (
            <p className="rounded border border-dashed border-slate-200 px-3 py-4 text-center text-xs text-slate-400 dark:border-slate-700">
              Drag an account here
            </p>
          )}
        </div>
        <div className="flex flex-wrap gap-2 px-3 pb-3 pt-2 pl-10">
          <button
            type="button"
            disabled={!!active}
            onClick={() => onImport("claude")}
            className={actionClass}
          >
            + Import claude
          </button>
          <button
            type="button"
            disabled={!!active}
            onClick={() => onImport("codex")}
            className={actionClass}
          >
            + Import codex
          </button>
          {referenceable.length > 0 && (
            <select
              disabled={!!active}
              value=""
              onChange={(event) => {
                if (event.target.value) onReference(event.target.value);
              }}
              className={`${actionClass} min-w-0 max-w-full dark:bg-slate-900`}
              aria-label={`reference an existing account into ${group.name || "this group"}`}
            >
              <option value="">+ Clone an existing account…</option>
              {referenceable.map((account) => (
                <option key={account.id} value={account.email}>
                  {account.email}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>
    </section>
  );
}

function MemberRow({
  item,
  provider,
  disabled,
  lastGroup,
  onRemove,
}: {
  item: Extract<TreeDragItem, { kind: "member" }>;
  provider: string;
  disabled: boolean;
  lastGroup: boolean;
  onRemove: () => void;
}) {
  const { attributes, listeners, setNodeRef, setActivatorNodeRef, isDragging } =
    useDraggable({ id: itemId(item), data: { item } });
  return (
    <div
      ref={setNodeRef}
      data-account={item.email}
      className={`flex min-h-9 items-center gap-2 rounded border border-slate-200 px-2 py-1 text-xs text-slate-700 dark:border-slate-700 dark:text-slate-200 ${isDragging ? "opacity-40" : ""}`}
    >
      <button
        ref={setActivatorNodeRef}
        data-drag-handle={itemId(item)}
        type="button"
        {...attributes}
        {...listeners}
        aria-label={`reorder ${item.email}`}
        className={gripClass}
      >
        <GripVertical size={14} />
      </button>
      <AccountLabel email={item.email} provider={provider} />
      <button
        type="button"
        disabled={disabled}
        onClick={onRemove}
        title={
          lastGroup
            ? "Remove from its last group — saving deletes this account entirely"
            : "Remove from this group (it stays in its other groups)"
        }
        aria-label={`remove ${item.email} from this group`}
        className="shrink-0 rounded p-1 text-slate-400 hover:bg-slate-100 disabled:opacity-40 dark:hover:bg-slate-800"
      >
        <X size={14} />
      </button>
    </div>
  );
}

function AccountLabel({
  email,
  provider,
}: {
  email: string;
  provider: string;
}) {
  return (
    <>
      {provider === "claude" || provider === "codex" ? (
        <img
          src={provider === "codex" ? chatgptLogo : claudeLogo}
          alt={provider === "codex" ? "ChatGPT" : "Claude"}
          className={`h-4 w-4 shrink-0 object-contain ${provider === "codex" ? "dark:invert" : ""}`}
        />
      ) : (
        <span className="text-slate-400" title="Unknown provider">
          ?
        </span>
      )}
      <span title={email} className="min-w-0 flex-1 truncate">
        {email}
      </span>
    </>
  );
}
