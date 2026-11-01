// The /ng Clones page: the dashboard's board, as one list with a section per column. A tap on
// a clone puts it on the screen above (the server streams the selected clone); an archived
// clone has no screen, so a tap on one opens its notes instead. The control panel stays open; a double
// click (a double tap) also closes them, so the clone's screen shows.
//
// The card menu, the drags and the column buttons of the desktop are here as: the ⋯ on a row
// opens a sheet with all the card's actions, "Move to" a column (into an archive column
// archives, out of one restores, as the drop does), and Move up / Move down in its column; the
// "+" on a section makes a clone in that column. Columns are renamed, added and ordered in
// Settings, Board, as on the desktop.
//
// Wide (`columnsLayout`), the sections are the board's columns side by side, with the
// dashboard's own card per clone (`SidebarClone`), on the control panel's glass.
//
// As on the dashboard: a clone drags within its column or into another one (`./BoardDnd.tsx`:
// a mouse at once, a finger after a long press), a double-click on a column's title renames it,
// and a mouse's click on ⋯ opens the dashboard's menu; a finger's opens the sheet.
//
// Pure: everything comes in as props, every action goes out as a callback.
import {
  Archive,
  ArchiveRestore,
  ArrowDown,
  ArrowUp,
  ArrowUpToLine,
  Bell,
  BellOff,
  ChevronDown,
  ChevronRight,
  Columns3,
  Ellipsis,
  ExternalLink,
  Layers,
  Network,
  Pencil,
  Plus,
  Terminal,
  Trash2,
  UserCog,
} from "lucide-react";
import { type MouseEvent, useState } from "react";

import type { Box } from "~/components/OverflowMenu";
import { SidebarClone } from "~/components/SidebarClone";

import chatgptLogo from "~/assets/chatgpt.svg";
import claudeLogo from "~/assets/claude.svg";
import { CloneStatusDot, statusLabel } from "~/components/mobile/CloneStatus";
import { formatCloneUsage, RenameField } from "~/components/SidebarClone";
import { cloneTitle } from "~/lib/cloneTitle";
import { type BoardColumn, resolveColumns, subCloneTree, withDefaults } from "~/lib/board";
import { formatTokenCount } from "~/lib/format";
import { cloneIndex, isMutedByParent, mutedSet } from "~/lib/mute";
import type { CloneTicket } from "~/lib/tickets";
import type { Clone, Operation } from "~/lib/types";
import type { CloneTokens } from "~/lib/wire/CloneTokens";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import { workspaceBadge } from "~/lib/workspace";

import { DragItem, DropColumn, useBoardDnd } from "./BoardDnd";
import { type Move, movesFor } from "./order";
import {
  ActionSheet,
  BOX,
  boxOf,
  CARD,
  CARD_BOX,
  CARD_LIST,
  DesktopMenu,
  DIVIDE,
  EmptyLine,
  FIELD,
  HERE,
  isMouseClick,
  NO_DRAG,
  ON_GLASS,
  Page,
  PageHeader,
  PRESS,
  R_PIECE,
  type SheetGroup,
  TEXT,
  TEXT_2,
  TEXT_3,
  useColumnFrame,
} from "./ui";

export interface ClonesViewProps {
  /** The board's columns (`ControlState.boardColumns`); empty gives the defaults. */
  columns: BoardColumn[];
  /** Every clone, archived ones too. */
  clones: Clone[];
  cloneTickets: Record<string, CloneTicket>;
  stats: Record<string, ContainerStats>;
  cloneTokens: Record<string, CloneTokens>;
  operations: Operation[];
  mutedClones: string[];
  /** The clone on the screen above. */
  selectedId: string | null;
  /** The clone the Notes page is on, when it is not that one (an archived one). */
  notesId: string | null;
  /** A tap on a clone: a live one goes on the screen, an archived one opens its notes. */
  onSelect: (clone: Clone) => void;
  /** A double click on a clone: as a tap, then the control panel closes (the clone's screen shows). */
  onOpen: (clone: Clone) => void;
  /** The clone's display name, the operator's own (empty: none); the hostname stays. */
  onRename: (clone: Clone, name: string) => void;
  onNewClone: (columnId: string) => void;
  /** Into `columnId`, at its end. The container archives or restores it when the column says. */
  onMoveToColumn: (clone: Clone, columnId: string) => void;
  onMove: (clone: Clone, columnId: string, move: Move) => void;
  onOpenInLinear: (url: string) => void;
  onChangeAccount: (clone: Clone) => void;
  onRebase: (clone: Clone) => void;
  onPortForward: (clone: Clone) => void;
  /** Copy the clone's SSH command; answers whether it landed. */
  onCopySsh: (clone: Clone) => Promise<boolean>;
  onToggleMute: (clone: Clone) => void;
  onArchive: (clone: Clone) => void;
  onUnarchive: (clone: Clone) => void;
  onDelete: (clone: Clone) => void;
  /** The board's columns side by side (wide), not one list with a section each. */
  columnsLayout?: boolean;
  /** A column's new title (a double-click on it). Absent: titles are not renamed here. */
  onRenameColumn?: (columnId: string, title: string) => void;
}

const MOVE_ACTION = {
  top: { icon: ArrowUpToLine, label: "Move to top" },
  up: { icon: ArrowUp, label: "Move up" },
  down: { icon: ArrowDown, label: "Move down" },
} as const;

export function ClonesView(p: ClonesViewProps) {
  const frame = useColumnFrame();
  const dnd = useBoardDnd();
  const [sheet, setSheet] = useState<Clone | null>(null);
  /** The dashboard's menu, opened by a mouse, and where. */
  const [menu, setMenu] = useState<{ clone: Clone; at: Box } | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; title: string } | null>(null);
  /** The clone whose title is a field for a new name (Rename… in its menu). */
  const [renamingClone, setRenamingClone] = useState<string | null>(null);
  /** The field is done: a new name (a changed one only), or null (Escape). */
  const renameDone = (clone: Clone, title: string) => (name: string | null) => {
    setRenamingClone(null);
    if (name !== null && name.trim() !== title) p.onRename(clone, name.trim());
  };
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  // While a clone is in the air, the columns as it would leave them
  const columns = resolveColumns(withDefaults(p.columns), p.clones).map((c) => ({
    ...c,
    cloneIds: dnd?.lanes?.find((l) => l.id === c.id)?.cloneIds ?? c.cloneIds,
  }));
  /** One clone is made at a time: the columns' "New clone" waits while one is. */
  const cloning = p.operations.some((o) => o.kind === "clone" && o.status === "running");
  const openMenu = (clone: Clone, e: MouseEvent) => (isMouseClick(e) ? setMenu({ clone, at: boxOf(e) }) : setSheet(clone));
  const byId = cloneIndex(p.clones);
  const muted = mutedSet(p.mutedClones);
  const { childrenByParent } = subCloneTree(p.clones);
  const running = new Map(
    p.operations.filter((o) => o.status === "running").map((o) => [o.target, o]),
  );

  const toggle = (id: string) =>
    setExpanded((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const row = (clone: Clone, child: boolean) => {
    const kids = childrenByParent.get(clone.id) ?? [];
    return (
      <CloneRow
        key={clone.id}
        clone={clone}
        ticket={p.cloneTickets[clone.id]}
        stats={p.stats[clone.id]}
        tokens={p.cloneTokens[clone.id]}
        op={running.get(clone.id)}
        selected={clone.id === p.selectedId}
        notes={clone.id === p.notesId}
        muted={muted.has(clone.id) || isMutedByParent(clone, byId, muted)}
        child={child}
        childCount={kids.length}
        expanded={expanded.has(clone.id)}
        onToggleExpand={() => toggle(clone.id)}
        onSelect={() => p.onSelect(clone)}
        onOpen={() => p.onOpen(clone)}
        renaming={renamingClone === clone.id}
        onRenameDone={renameDone(clone, cloneTitle(clone, p.cloneTickets[clone.id]))}
        onMenu={(e) => openMenu(clone, e)}
      />
    );
  };

  /** The dashboard's card for a clone (wide), with its sub clones in the same frame. */
  const card = (clone: Clone, drag?: Record<string, unknown>) => {
    const kids = childrenByParent.get(clone.id) ?? [];
    const props = (c: Clone, child: boolean) => ({
      clone: c,
      ticket: p.cloneTickets[c.id],
      stats: p.stats[c.id],
      tokens: p.cloneTokens[c.id],
      op: running.get(c.id),
      selected: c.id === p.selectedId,
      onSelect: () => p.onSelect(c),
      onOpen: () => p.onOpen(c),
      onDelete: () => p.onDelete(c),
      onChangeAccount: () => p.onChangeAccount(c),
      onRebase: () => p.onRebase(c),
      onPortForward: () => p.onPortForward(c),
      onArchive: () => p.onArchive(c),
      onUnarchive: () => p.onUnarchive(c),
      onCopySshCommand: () => p.onCopySsh(c),
      onOpenInLinear: p.onOpenInLinear,
      muted: muted.has(c.id),
      mutedByParent: isMutedByParent(c, byId, muted),
      onToggleMute: () => p.onToggleMute(c),
      onRename: (name: string) => p.onRename(c, name),
      renaming: renamingClone === c.id,
      onRenamingChange: (on: boolean) => setRenamingClone(on ? c.id : null),
      sshPublicHost: "",
      isChild: child,
      fill: {
        idle: "bg-transparent hover:bg-black/[0.03] dark:hover:bg-white/5",
        selected: "bg-emerald-500/10 dark:bg-emerald-400/10",
      },
      renameField: `mt-1.5 w-full px-1.5 py-0.5 text-sm font-medium ${FIELD}`,
      touchScroll: true,
      onMenuTouch: () => setSheet(c),
    });
    return (
      <div className={`overflow-hidden ${CARD_BOX} ${p.notesId === clone.id ? BOX : ""}`}>
        <SidebarClone
          {...props(clone, false)}
          childCount={kids.length}
          expanded={expanded.has(clone.id)}
          onToggleExpand={() => toggle(clone.id)}
          dragListeners={drag as Parameters<typeof SidebarClone>[0]["dragListeners"]}
        />
        {expanded.has(clone.id)
          ? kids.map((k) => (
              <div key={k.id} className="border-t border-black/10 dark:border-white/10">
                <SidebarClone {...props(k, true)} />
              </div>
            ))
          : null}
      </div>
    );
  };
  // What follows the pointer while a clone is dragged
  if (dnd) {
    dnd.overlays.current.clone = (id) => {
      const clone = byId.get(id);
      if (!clone) return null;
      return p.columnsLayout ? card(clone) : <ul>{row(clone, false)}</ul>;
    };
  }

  const groupsFor = (clone: Clone): SheetGroup[] => {
    const managed = clone.managed === true;
    const archived = clone.archived === true;
    const url = p.cloneTickets[clone.id]?.url ?? clone.linearTicketUrl ?? undefined;
    const byParent = isMutedByParent(clone, byId, muted);
    const home = columns.find((c) => c.cloneIds.includes(clone.id));
    const groups: SheetGroup[] = [
      {
        actions: [
          { icon: Pencil, label: "Rename", onSelect: () => setRenamingClone(clone.id) },
          ...(url ? [{ icon: ExternalLink, label: "Open in Linear", onSelect: () => p.onOpenInLinear(url) }] : []),
          ...(managed && !archived
            ? [
                { icon: UserCog, label: "Change account", onSelect: () => p.onChangeAccount(clone) },
                { icon: Layers, label: "Rebase", onSelect: () => p.onRebase(clone) },
                { icon: Network, label: "Port forward", onSelect: () => p.onPortForward(clone) },
                { icon: Terminal, label: "Copy SSH command", onSelect: () => p.onCopySsh(clone) },
              ]
            : []),
          ...(byParent
            ? []
            : [
                {
                  icon: muted.has(clone.id) ? Bell : BellOff,
                  label: muted.has(clone.id) ? "Unmute" : "Mute",
                  onSelect: () => p.onToggleMute(clone),
                },
              ]),
        ],
      },
    ];
    // A sub clone hangs under its parent and moves with it, so it has no column of its own
    if (home) {
      groups.push(
        {
          title: "Move to column",
          actions: columns.map((c) => ({
            icon: c.archive ? Archive : Columns3,
            label: c.title,
            current: c.id === home.id,
            onSelect: () => (c.id === home.id ? undefined : p.onMoveToColumn(clone, c.id)),
          })),
        },
        {
          title: "Order",
          actions: movesFor(home.cloneIds, clone.id).map((m) => ({
            ...MOVE_ACTION[m],
            onSelect: () => p.onMove(clone, home.id, m),
          })),
        },
      );
    }
    groups.push({
      actions: [
        ...(managed && !archived ? [{ icon: Archive, label: "Archive", onSelect: () => p.onArchive(clone) }] : []),
        ...(managed && archived
          ? [{ icon: ArchiveRestore, label: "Unarchive", onSelect: () => p.onUnarchive(clone) }]
          : []),
        { icon: Trash2, label: managed ? "Delete" : "Remove", danger: true, onSelect: () => p.onDelete(clone) },
      ],
    });
    return groups;
  };

  const sheetView = (
    <>
      {sheet ? (
        <ActionSheet
          title={cloneTitle(sheet, p.cloneTickets[sheet.id])}
          groups={groupsFor(sheet)}
          onClose={() => setSheet(null)}
        />
      ) : null}
      {menu ? <DesktopMenu at={menu.at} groups={groupsFor(menu.clone)} onClose={() => setMenu(null)} /> : null}
    </>
  );
  /** A column's clones: each one with its sub clones, as one thing that drags. */
  const items = (column: (typeof columns)[number]) =>
    column.cloneIds.flatMap((id) => {
      const clone = byId.get(id);
      if (!clone) return [];
      const kids = expanded.has(id) ? (childrenByParent.get(id) ?? []) : [];
      const busy = running.has(id);
      return [
        <DragItem key={id} id={id} disabled={busy}>
          {(drag) =>
            p.columnsLayout ? (
              card(clone, drag)
            ) : (
              <ul {...drag} className={`divide-y ${DIVIDE}`}>
                {row(clone, false)}
                {kids.map((k) => row(k, true))}
              </ul>
            )
          }
        </DragItem>,
      ];
    });
  const newClone = (column: (typeof columns)[number]) => (
    <button
      type="button"
      onClick={() => p.onNewClone(column.id)}
      disabled={cloning}
      aria-label={`New clone in ${column.title}`}
      title={`Create a clone in ${column.title}`}
      className={`flex h-7 shrink-0 items-center gap-1 ${R_PIECE} px-2 text-xs font-medium normal-case tracking-normal disabled:opacity-40 ${TEXT_2} ${PRESS}`}
    >
      <Plus aria-hidden className="size-3.5" />
      New clone
    </button>
  );
  const heading = (column: (typeof columns)[number]) =>
    renaming?.id === column.id ? (
      <form
        className="flex min-w-0 flex-1 items-center gap-1 normal-case tracking-normal"
        onSubmit={(e) => {
          e.preventDefault();
          const next = renaming.title.trim();
          if (next && next !== column.title) p.onRenameColumn?.(column.id, next);
          setRenaming(null);
        }}
      >
        <input
          autoFocus
          value={renaming.title}
          onChange={(e) => setRenaming({ id: column.id, title: e.target.value })}
          onKeyDown={(e) => e.key === "Escape" && setRenaming(null)}
          onBlur={() => setRenaming(null)}
          aria-label="Column name"
          className={`min-w-0 flex-1 ${R_PIECE} border border-black/10 bg-white/60 px-2 py-0.5 text-sm font-normal focus:border-black/30 focus:outline-none dark:border-white/10 dark:bg-black/30 dark:focus:border-white/30 ${TEXT}`}
        />
      </form>
    ) : (
      <>
        <span
          className="truncate"
          title={p.onRenameColumn ? "Double-click to rename" : undefined}
          onDoubleClick={() => p.onRenameColumn && setRenaming({ id: column.id, title: column.title })}
        >
          {column.title}
        </span>
        <span className={`tabular-nums ${TEXT_3}`}>{column.cloneIds.length}</span>
        <span className="flex-1" />
        {newClone(column)}
      </>
    );

  if (p.columnsLayout) {
    return (
      <>
        {columns.map((column) => (
          <section
            key={column.id}
            aria-label={column.title}
            className={`flex h-full w-72 shrink-0 flex-col ${frame} ${ON_GLASS}`}
          >
            <h3 className={`flex h-10 shrink-0 items-center gap-2 pl-3 pr-1.5 text-sm font-semibold ${TEXT}`}>{heading(column)}</h3>
            <DropColumn
              id={column.id}
              items={column.cloneIds}
              className={`min-h-0 flex-1 overflow-y-auto overscroll-y-contain ${CARD_LIST}`}
            >
              {items(column)}
              {column.cloneIds.length === 0 ? (
                <p className={`rounded-lg border border-dashed border-black/15 p-4 text-center text-xs dark:border-white/15 ${TEXT_3}`}>
                  Drop a clone here.
                </p>
              ) : null}
            </DropColumn>
          </section>
        ))}
        {sheetView}
      </>
    );
  }

  return (
    <div className="relative h-full">
      <Page header={<PageHeader title="Clones" sub={`${p.clones.length} clones`} />}>
        {columns.every((c) => c.cloneIds.length === 0) && p.clones.length === 0 ? (
          <EmptyLine>No clones yet. Make one with the + of a column.</EmptyLine>
        ) : null}
        {columns.map((column) => (
          <DropColumn key={column.id} id={column.id} items={column.cloneIds} as="section">
            {/* Frosted over the rows that scroll beneath it */}
            <h3
              className={`sticky top-0 z-10 flex h-8 items-center gap-2 bg-white/50 pl-3 pr-1 text-[11px] font-semibold uppercase tracking-wide backdrop-blur-md dark:bg-black/30 ${TEXT_2}`}
            >
              {heading(column)}
            </h3>
            <ul className={`divide-y ${DIVIDE}`}>{items(column)}</ul>
          </DropColumn>
        ))}
      </Page>
      {sheetView}
    </div>
  );
}

/** One clone: its state, name and ticket on the first line, its accounts and numbers on the
 *  second. The one on the screen is lit. */
export function CloneRow({
  clone,
  ticket,
  stats,
  tokens,
  op,
  selected,
  notes,
  muted,
  child,
  childCount,
  expanded,
  onToggleExpand,
  onSelect,
  onOpen,
  renaming,
  onRenameDone,
  onMenu,
}: {
  clone: Clone;
  ticket?: CloneTicket;
  stats?: ContainerStats;
  tokens?: CloneTokens;
  /** A running operation on it: its step shows, and the menu waits. */
  op?: Operation;
  selected: boolean;
  /** The Notes page is on it. */
  notes: boolean;
  muted: boolean;
  child: boolean;
  childCount: number;
  expanded: boolean;
  onToggleExpand: () => void;
  onSelect: () => void;
  onOpen: () => void;
  /** The title is a field for a new name; `onRenameDone` takes it (null: Escape). */
  renaming: boolean;
  onRenameDone: (name: string | null) => void;
  onMenu: (e: MouseEvent<HTMLButtonElement>) => void;
}) {
  const title = cloneTitle(clone, ticket);
  const usage = clone.archived ? null : (formatCloneUsage(stats) ?? { cpu: "0%", mem: "0.0GB" });
  const busy = op !== undefined;
  const marks = (
    <>
      <CloneStatusDot clone={clone} />
      {muted ? <BellOff aria-label="muted" className={`size-3 shrink-0 ${TEXT_3}`} /> : null}
      {clone.linearTicket ? (
        <span
          className={`shrink-0 rounded px-1 py-0.5 text-[10px] font-semibold leading-none ${workspaceBadge(
            clone.linearWorkspace ?? clone.linearTicket.split("-")[0].toLowerCase(),
          )}`}
        >
          {clone.linearTicket}
        </span>
      ) : null}
      {clone.headless ? <Terminal aria-label="headless" className={`size-3.5 shrink-0 ${TEXT_2}`} /> : null}
    </>
  );
  const state = busy ? (
    <span className="shrink-0 text-[11px] font-medium text-sky-600 dark:text-sky-400">
      {op.kind === "delete" ? "deleting…" : op.step}
    </span>
  ) : usage ? (
    <span className={`shrink-0 font-mono text-[11px] tabular-nums ${TEXT_2}`}>
      {usage.cpu} · {usage.mem}
    </span>
  ) : (
    <span className={`shrink-0 text-[11px] ${TEXT_3}`}>{statusLabel(clone)}</span>
  );
  const tokensLine = (
    <span className="shrink-0 font-mono tabular-nums">
      ↑{formatTokenCount(tokens?.inputTokens ?? 0)} ↓{formatTokenCount(tokens?.outputTokens ?? 0)}
    </span>
  );
  const subs =
    childCount > 0 ? (
      <button
        type="button"
        {...NO_DRAG}
        onClick={onToggleExpand}
        aria-expanded={expanded}
        aria-label={`${expanded ? "Hide" : "Show"} ${childCount} sub clones`}
        className={`flex shrink-0 items-center gap-0.5 px-1.5 text-[11px] ${TEXT_2} ${PRESS}`}
      >
        {expanded ? <ChevronDown aria-hidden className="size-3.5" /> : <ChevronRight aria-hidden className="size-3.5" />}
        {childCount}
      </button>
    ) : null;
  const menu = (
    <button
      type="button"
      {...NO_DRAG}
      onClick={onMenu}
      disabled={busy}
      aria-label={`Actions for ${clone.id}`}
      className={`flex w-11 shrink-0 items-center justify-center ${TEXT_2} ${PRESS} disabled:opacity-30`}
    >
      <Ellipsis aria-hidden className="size-4" />
    </button>
  );

  return (
    <li className={`flex items-stretch ${selected ? HERE : notes ? BOX : ""}`}>
      {renaming ? (
        <div className={`flex min-w-0 flex-1 items-center gap-1.5 py-1.5 pr-2 ${child ? "pl-8" : "pl-3"}`}>
          {marks}
          <RenameField initial={title} onDone={onRenameDone} className={`min-w-0 flex-1 px-1.5 py-0.5 text-sm ${FIELD}`} />
        </div>
      ) : (
        <button
          type="button"
          onClick={onSelect}
          onDoubleClick={onOpen}
          aria-current={selected ? "true" : undefined}
          className={`min-w-0 flex-1 py-1.5 pr-1 text-left ${PRESS} ${child ? "pl-8" : "pl-3"}`}
        >
          <span className="flex items-center gap-1.5">
            {marks}
            <span className={`min-w-0 flex-1 truncate text-sm ${TEXT}`}>{title}</span>
            {state}
          </span>
          <span className={`mt-0.5 flex items-center gap-2 text-[11px] ${TEXT_3}`}>
            {clone.managed ? (
              <>
                <Account logo={claudeLogo} email={clone.claudeAccountEmail} selection={clone.claudeSelection} group={clone.group ?? clone.claudeGroup} />
                <Account logo={chatgptLogo} invert email={clone.codexAccountEmail} selection={clone.codexSelection} group={clone.group ?? clone.codexGroup} />
              </>
            ) : (
              <span className="min-w-0 flex-1 truncate">{clone.id}</span>
            )}
            {tokensLine}
          </span>
        </button>
      )}
      {subs}
      {menu}
    </li>
  );
}

/** A provider's account on a clone: the logo, the pool or "auto" when the server picks, and
 *  the account in use. */
function Account({
  logo,
  invert = false,
  email,
  selection,
  group,
}: {
  logo: string;
  invert?: boolean;
  email: string | null | undefined;
  selection: string | null | undefined;
  group: string | null | undefined;
}) {
  const badge = group ?? (selection === "auto" ? "auto" : null);
  return (
    <span className="flex min-w-0 flex-1 items-center gap-1">
      <img src={logo} alt="" className={`size-3 shrink-0 opacity-70 ${invert ? "dark:invert" : ""}`} />
      {badge ? <span className={`shrink-0 rounded bg-black/5 px-1 text-[10px] dark:bg-white/10 ${TEXT_2}`}>{badge}</span> : null}
      <span className={`min-w-0 truncate ${email ? TEXT_2 : `italic ${TEXT_3}`}`}>
        {email ?? (selection === "none" ? "no token" : "unassigned")}
      </span>
    </span>
  );
}
