// The /ng Clones page: the dashboard's board, as one list with a section per column. A tap on
// a clone puts it on the screen above (the server streams the selected clone); an archived
// clone has no screen, so a tap on one opens its notes instead.
//
// The card menu, the drags and the column buttons of the desktop are here as: the ⋯ on a row
// opens a sheet with all the card's actions, "Move to" a column (into an archive column
// archives, out of one restores, as the drop does), and Move up / Move down in its column; the
// "+" on a section makes a clone in that column. Columns are renamed, added and ordered in
// Settings, Board, as on the desktop.
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
  Plus,
  Terminal,
  Trash2,
  UserCog,
} from "lucide-react";
import { useState } from "react";

import chatgptLogo from "~/assets/chatgpt.svg";
import claudeLogo from "~/assets/claude.svg";
import { CloneStatusDot, statusLabel } from "~/components/mobile/CloneStatus";
import { formatCloneUsage } from "~/components/SidebarClone";
import { type BoardColumn, resolveColumns, subCloneTree, withDefaults } from "~/lib/board";
import { formatTokenCount } from "~/lib/format";
import { cloneIndex, isMutedByParent, mutedSet } from "~/lib/mute";
import type { CloneTicket } from "~/lib/tickets";
import type { Clone, Operation } from "~/lib/types";
import type { CloneTokens } from "~/lib/wire/CloneTokens";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import { workspaceBadge } from "~/lib/workspace";

import { type Move, movesFor } from "./order";
import { ActionSheet, EmptyLine, Page, PageHeader, type SheetGroup } from "./ui";

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
}

const MOVE_ACTION = {
  top: { icon: ArrowUpToLine, label: "Move to top" },
  up: { icon: ArrowUp, label: "Move up" },
  down: { icon: ArrowDown, label: "Move down" },
} as const;

export function ClonesView(p: ClonesViewProps) {
  const [sheet, setSheet] = useState<Clone | null>(null);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const columns = resolveColumns(withDefaults(p.columns), p.clones);
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
        onMenu={() => setSheet(clone)}
      />
    );
  };

  const groupsFor = (clone: Clone): SheetGroup[] => {
    const managed = clone.managed === true;
    const archived = clone.archived === true;
    const url = p.cloneTickets[clone.id]?.url ?? clone.linearTicketUrl ?? undefined;
    const byParent = isMutedByParent(clone, byId, muted);
    const home = columns.find((c) => c.cloneIds.includes(clone.id));
    const groups: SheetGroup[] = [
      {
        actions: [
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

  return (
    <div className="relative h-full">
      <Page
        header={<PageHeader title="Clones" sub={`${p.clones.length} clones`} />}
      >
        {columns.every((c) => c.cloneIds.length === 0) && p.clones.length === 0 ? (
          <EmptyLine>No clones yet. Make one with the + of a column.</EmptyLine>
        ) : null}
        {columns.map((column) => (
          <section key={column.id} aria-label={column.title}>
            <h3 className="sticky top-0 z-10 flex h-8 items-center gap-2 bg-black/90 pl-3 pr-1 text-[11px] font-semibold uppercase tracking-wide text-neutral-400 backdrop-blur">
              <span className="truncate">{column.title}</span>
              <span className="tabular-nums text-neutral-600">{column.cloneIds.length}</span>
              <span className="flex-1" />
              <button
                type="button"
                onClick={() => p.onNewClone(column.id)}
                aria-label={`New clone in ${column.title}`}
                className="flex h-7 items-center gap-1 rounded-md px-2 text-xs font-medium normal-case tracking-normal text-neutral-300 active:bg-neutral-800"
              >
                <Plus aria-hidden className="size-3.5" />
                New clone
              </button>
            </h3>
            <ul className="divide-y divide-neutral-900">
              {column.cloneIds.flatMap((id) => {
                const clone = byId.get(id);
                if (!clone) return [];
                const kids = expanded.has(id) ? (childrenByParent.get(id) ?? []) : [];
                return [row(clone, false), ...kids.map((k) => row(k, true))];
              })}
            </ul>
          </section>
        ))}
      </Page>
      {sheet ? (
        <ActionSheet
          title={p.cloneTickets[sheet.id]?.title ?? sheet.displayName ?? sheet.id}
          groups={groupsFor(sheet)}
          onClose={() => setSheet(null)}
        />
      ) : null}
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
  onMenu: () => void;
}) {
  const title = ticket?.title ?? clone.displayName ?? clone.id;
  const usage = clone.archived ? null : (formatCloneUsage(stats) ?? { cpu: "0%", mem: "0.0GB" });
  const busy = op !== undefined;
  return (
    <li
      className={`flex items-stretch ${selected ? "bg-emerald-950/60" : notes ? "bg-neutral-900" : ""} ${
        selected ? "shadow-[inset_3px_0_0_var(--color-emerald-400)]" : ""
      }`}
    >
      <button
        type="button"
        onClick={onSelect}
        aria-current={selected ? "true" : undefined}
        className={`min-w-0 flex-1 py-1.5 pr-1 text-left active:bg-neutral-800 ${child ? "pl-8" : "pl-3"}`}
      >
        <span className="flex items-center gap-1.5">
          <CloneStatusDot clone={clone} />
          {muted ? <BellOff aria-label="muted" className="size-3 shrink-0 text-neutral-500" /> : null}
          {clone.linearTicket ? (
            <span
              className={`shrink-0 rounded px-1 py-0.5 text-[10px] font-semibold leading-none ${workspaceBadge(
                clone.linearWorkspace ?? clone.linearTicket.split("-")[0].toLowerCase(),
              )}`}
            >
              {clone.linearTicket}
            </span>
          ) : null}
          {clone.headless ? <Terminal aria-label="headless" className="size-3.5 shrink-0 text-neutral-400" /> : null}
          <span className="min-w-0 flex-1 truncate text-sm text-neutral-100">{title}</span>
          {busy ? (
            <span className="shrink-0 text-[11px] font-medium text-sky-400">
              {op.kind === "delete" ? "deleting…" : op.step}
            </span>
          ) : usage ? (
            <span className="shrink-0 font-mono text-[11px] tabular-nums text-neutral-400">
              {usage.cpu} · {usage.mem}
            </span>
          ) : (
            <span className="shrink-0 text-[11px] text-neutral-500">{statusLabel(clone)}</span>
          )}
        </span>
        <span className="mt-0.5 flex items-center gap-2 text-[11px] text-neutral-500">
          {clone.managed ? (
            <>
              <Account logo={claudeLogo} email={clone.claudeAccountEmail} selection={clone.claudeSelection} group={clone.group ?? clone.claudeGroup} />
              <Account logo={chatgptLogo} invert email={clone.codexAccountEmail} selection={clone.codexSelection} group={clone.group ?? clone.codexGroup} />
            </>
          ) : (
            <span className="min-w-0 flex-1 truncate">{clone.id}</span>
          )}
          <span className="shrink-0 font-mono tabular-nums">
            ↑{formatTokenCount(tokens?.inputTokens ?? 0)} ↓{formatTokenCount(tokens?.outputTokens ?? 0)}
          </span>
        </span>
      </button>
      {childCount > 0 ? (
        <button
          type="button"
          onClick={onToggleExpand}
          aria-expanded={expanded}
          aria-label={`${expanded ? "Hide" : "Show"} ${childCount} sub clones`}
          className="flex shrink-0 items-center gap-0.5 px-1.5 text-[11px] text-neutral-400 active:bg-neutral-800"
        >
          {expanded ? <ChevronDown aria-hidden className="size-3.5" /> : <ChevronRight aria-hidden className="size-3.5" />}
          {childCount}
        </button>
      ) : null}
      <button
        type="button"
        onClick={onMenu}
        disabled={busy}
        aria-label={`Actions for ${clone.id}`}
        className="flex w-11 shrink-0 items-center justify-center text-neutral-400 active:bg-neutral-800 disabled:opacity-30"
      >
        <Ellipsis aria-hidden className="size-4" />
      </button>
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
      <img src={logo} alt="" className={`size-3 shrink-0 opacity-70 ${invert ? "invert" : ""}`} />
      {badge ? <span className="shrink-0 rounded bg-neutral-800 px-1 text-[10px] text-neutral-300">{badge}</span> : null}
      <span className={`min-w-0 truncate ${email ? "text-neutral-400" : "italic text-neutral-600"}`}>
        {email ?? (selection === "none" ? "no token" : "unassigned")}
      </span>
    </span>
  );
}
