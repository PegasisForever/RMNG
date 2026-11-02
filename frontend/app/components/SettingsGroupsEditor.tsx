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
import { DropdownSelect } from "~/components/DropdownSelect";
import { useModalSurface } from "~/components/ModalShell";
import { settingsInput } from "~/components/SettingsFields";
import { BUTTON, CARD_BOX, FIELD, FLOAT_SOLID, ICON_BUTTON, LINE, R_PIECE, TEXT, TEXT_3 } from "~/desk/control/ui";
import {
  applyTreeDrop,
  isDuplicateDrop,
  type EditorGroup,
  type TreeDragItem,
  type TreeDropTarget,
} from "~/lib/groupTreeDrag";
import { newGroup, type GroupDraft } from "~/lib/settingsDraft";
import type { ClaudeUsage } from "~/lib/types";
import type { GroupMember } from "~/lib/wire/GroupMember";
import type { Provider } from "~/lib/wire/Provider";
import {
  itemId,
  targetId,
  treeCollision,
  treeKeyboardCoordinates,
} from "./SettingsGroupsEditor.dnd";

const actionClass = `${BUTTON} px-2 py-1 text-xs disabled:opacity-40`;
// Keyboard focus: a dark outline in light, a white one in dark, as a glass control's
const gripClass = `shrink-0 cursor-grab touch-none p-1 ${ICON_BUTTON} focus-visible:outline-2 focus-visible:outline-neutral-900 active:cursor-grabbing dark:focus-visible:outline-white`;
/** A small icon button that removes something (a group, a member). */
const removeClass = `shrink-0 p-1 ${ICON_BUTTON} disabled:opacity-40`;

/** Which side a member row serves: untagged rows serve both. */
type Scope = Provider | "both";

function scopeOf(member: GroupMember): Scope {
  return member.provider ?? "both";
}

function providerOf(accounts: ClaudeUsage[], email: string): string {
  return (
    accounts.find((account) => account.email === email)?.provider ?? "unknown"
  );
}

/** Provider key for claimed/ungrouped bookkeeping: one email can name two accounts. */
function accountKey(email: string, provider: string): string {
  return `${email} ${provider}`;
}

/** The providers one member row covers. */
function coveredProviders(member: GroupMember): Provider[] {
  return member.provider ? [member.provider] : ["claude", "codex"];
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
  // Where the dragged piece floats: over the clone's screen (glass) or on a solid ground
  const surface = useModalSurface();
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
  const claimed = new Set(
    nodes.flatMap((group) =>
      group.accounts.flatMap((member) =>
        coveredProviders(member).map((p) => accountKey(member.email, p)),
      ),
    ),
  );
  const ungrouped = accounts.filter(
    (account) => !claimed.has(accountKey(account.email, account.provider ?? "")),
  );
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
                  nodes.filter((node) =>
                    node.accounts.some((m) => m.email === email),
                  ).length
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
                      (member) => member.email !== email,
                    ),
                  }))
                }
                onScopeChange={(email, scope) =>
                  updateGroup(group.id, (node) => ({
                    ...node,
                    accounts: node.accounts.map((member) =>
                      member.email === email
                        ? {
                            email: member.email,
                            ...(scope === "both" ? {} : { provider: scope }),
                          }
                        : member,
                    ),
                  }))
                }
                onReference={(email) => {
                  if (!group.accounts.some((m) => m.email === email))
                    updateGroup(group.id, (node) => ({
                      ...node,
                      accounts: [...node.accounts, { email }],
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
            <p className={`py-3 text-xs ${TEXT_3}`}>No groups.</p>
          )}
        </TreeRegion>
        {accounts.length === 0 && (
          <p className={`my-2 text-xs ${TEXT_3}`}>{noAccountsHint}</p>
        )}
        {ungrouped.length > 0 && (
          <p className="my-2 text-xs text-amber-700 dark:text-amber-300">
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
              // Lifted off the list: a piece of thick glass of its own, over the page; solid in
              // the control panel's own window, where no clone's screen is under it
              <div
                data-drag-preview
                className={`${R_PIECE} p-3 text-sm ${TEXT} ${surface === "solid" ? FLOAT_SOLID : "glass glass-thick"}`}
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
                  <p className="mt-1 text-xs text-amber-700 dark:text-amber-300">
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
          className={`pointer-events-none absolute inset-x-0 top-1/2 border-t-2 ${blocked ? "border-dashed border-amber-500" : "border-emerald-500"}`}
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
  onScopeChange,
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
  onScopeChange: (email: string, scope: Scope) => void;
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
    (account) => !group.accounts.some((m) => m.email === account.email),
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
        className={`${CARD_BOX} ${isDragging ? "opacity-40" : ""}`}
      >
        <div className="flex items-center gap-2 rounded-t-[10px] bg-black/[0.03] p-2 dark:bg-white/[0.03]">
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
            data-tip="Remove group"
            className={removeClass}
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
          {group.accounts.map((member, memberIndex) => (
            <Fragment key={member.email}>
              <MemberRow
                item={{ kind: "member", groupId: group.id, email: member.email }}
                scope={scopeOf(member)}
                provider={member.provider ?? providerOf(accounts, member.email)}
                disabled={!!active}
                lastGroup={memberships(member.email) <= 1}
                onRemove={() => onRemoveMember(member.email)}
                onScopeChange={(scope) => onScopeChange(member.email, scope)}
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
            <p className={`rounded-md border border-dashed px-3 py-4 text-center text-xs ${LINE} ${TEXT_3}`}>
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
            <DropdownSelect
              rows={[
                { value: "", label: "+ Clone an existing account…" },
                ...referenceable.map((account) => ({
                  value: account.email,
                  label: account.email,
                })),
              ]}
              value=""
              onChange={(picked) => {
                if (picked) onReference(picked);
              }}
              disabled={!!active}
              label={`reference an existing account into ${group.name || "this group"}`}
              className={`${actionClass} min-w-0 max-w-full`}
            />
          )}
        </div>
      </div>
    </section>
  );
}

function MemberRow({
  item,
  scope,
  provider,
  disabled,
  lastGroup,
  onRemove,
  onScopeChange,
}: {
  item: Extract<TreeDragItem, { kind: "member" }>;
  scope: Scope;
  provider: string;
  disabled: boolean;
  lastGroup: boolean;
  onRemove: () => void;
  onScopeChange: (scope: Scope) => void;
}) {
  const { attributes, listeners, setNodeRef, setActivatorNodeRef, isDragging } =
    useDraggable({ id: itemId(item), data: { item } });
  return (
    <div
      ref={setNodeRef}
      data-account={item.email}
      className={`flex min-h-9 items-center gap-2 rounded-md border px-2 py-1 text-xs ${LINE} ${TEXT} ${isDragging ? "opacity-40" : ""}`}
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
      <label className={`flex shrink-0 items-center gap-1 text-[10px] ${TEXT_3}`}>
        serves
        <select
          aria-label={`which side ${item.email} serves in this group`}
          disabled={disabled}
          value={scope}
          onChange={(event) => onScopeChange(event.target.value as Scope)}
          className={`${FIELD} px-1 py-0.5 text-[10px] disabled:opacity-40`}
        >
          <option value="both">both</option>
          <option value="claude">Claude</option>
          <option value="codex">Codex</option>
        </select>
      </label>
      <button
        type="button"
        disabled={disabled}
        onClick={onRemove}
        data-tip={
          lastGroup
            ? "Remove from its last group — saving deletes this account entirely"
            : "Remove from this group (it stays in its other groups)"
        }
        aria-label={`remove ${item.email} from this group`}
        className={removeClass}
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
        <span className={TEXT_3} data-tip="Unknown provider">
          ?
        </span>
      )}
      <span data-tip={email} className="min-w-0 flex-1 truncate">
        {email}
      </span>
    </>
  );
}
