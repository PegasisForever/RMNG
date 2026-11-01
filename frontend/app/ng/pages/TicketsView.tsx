// The /ng Tickets page: the dashboard's ticket column, as a list, and one ticket at a time in
// the same page. The open Linear tickets no clone was made for, in the operator's order.
//
// What the desktop does with the pointer is a tap here: a tap on a row opens the ticket, the
// ⋯ on a row opens a sheet of everything the card's menu does, and the drag that orders the
// column is Move up / Move down / Move to top in that sheet. A drag onto a clone column
// (which makes a clone) is "Create a clone" in the sheet.
//
// Wide (`column`), the list is the board's first column, as on the dashboard, and the open
// ticket shows beside the board (the container puts `detail` there).
//
// Pure: the open ticket's panel comes in as a slot (`detail`), because the real one loads its
// description editor lazily, in the browser only.
import {
  ArrowDown,
  ArrowUp,
  ArrowUpToLine,
  CircleDashed,
  CircleX,
  Ellipsis,
  ExternalLink,
  GitBranch,
  Link2,
  Plus,
} from "lucide-react";
import { type ReactNode, useState } from "react";

import { LabelPill, PriorityIcon, StateIcon } from "~/components/TicketColumn";
import type { LinearWorkspace } from "~/lib/linear/types";
import { branchNameOf, type LinearTicket } from "~/lib/tickets";
import { workspaceBadge } from "~/lib/workspace";

import { type Move, movesFor } from "./order";
import { ActionSheet, CARD, CARD_FILL, DIVIDE, EmptyLine, HeaderButton, LINE, useColumnFrame, Page, PageHeader, PRESS, type SheetGroup, TEXT, TEXT_2, TEXT_3 } from "./ui";

export interface TicketsViewProps {
  /** Open tickets no clone has, in the operator's order (`openTickets`, `orderTickets`). */
  tickets: LinearTicket[];
  /** No answer from Linear yet. */
  loading: boolean;
  /** Linear's last answer failed; the list keeps what it had. */
  error: string | null;
  /** The ticket shown in the page, and its panel; null shows the list. */
  open: LinearTicket | null;
  detail?: ReactNode;
  onOpen: (ticket: LinearTicket) => void;
  onBack: () => void;
  /** Absent ⇒ no "New" button (no Linear key). */
  onNewTicket?: () => void;
  /** The workspaces of the configured keys, for the "Linear" button. */
  workspaces: LinearWorkspace[];
  onOpenWorkspace: (workspace: LinearWorkspace) => void;
  onCreateClone: (ticket: LinearTicket) => void;
  onOpenInLinear: (url: string) => void;
  /** Put text on the clipboard; answers whether it landed. */
  onCopy: (text: string) => Promise<boolean>;
  onMove: (ticket: LinearTicket, move: Move) => void;
  onMoveToBacklog: (ticket: LinearTicket) => void;
  onCancel: (ticket: LinearTicket) => void;
  /** The board's first column (wide): a card per ticket; the open one is lit, not shown here. */
  column?: boolean;
}

const MOVE_ACTION = {
  top: { icon: ArrowUpToLine, label: "Move to top" },
  up: { icon: ArrowUp, label: "Move up" },
  down: { icon: ArrowDown, label: "Move down" },
} as const;

export function TicketsView(p: TicketsViewProps) {
  const frame = useColumnFrame();
  const [sheet, setSheet] = useState<LinearTicket | "workspaces" | null>(null);
  const ids = p.tickets.map((t) => t.id);

  const groupsFor = (t: LinearTicket): SheetGroup[] => [
    {
      actions: [
        { icon: Plus, label: "Create a clone", onSelect: () => p.onCreateClone(t) },
        { icon: ExternalLink, label: "Open in Linear", onSelect: () => p.onOpenInLinear(t.url) },
        { icon: GitBranch, label: "Copy branch name", onSelect: () => p.onCopy(branchNameOf(t)) },
        { icon: Link2, label: "Copy link", onSelect: () => p.onCopy(t.url) },
      ],
    },
    {
      title: "Order",
      actions: movesFor(ids, t.id).map((m) => ({ ...MOVE_ACTION[m], onSelect: () => p.onMove(t, m) })),
    },
    {
      title: "State in Linear",
      actions: [
        { icon: CircleDashed, label: "Move to backlog", onSelect: () => p.onMoveToBacklog(t) },
        { icon: CircleX, label: "Cancel ticket", danger: true, onSelect: () => p.onCancel(t) },
      ],
    },
  ];

  const sheetView =
    sheet === "workspaces" ? (
      <ActionSheet
        title="Open a Linear workspace"
        groups={[
          {
            actions: p.workspaces.map((w) => ({
              icon: ExternalLink,
              label: w.name,
              onSelect: () => p.onOpenWorkspace(w),
            })),
          },
        ]}
        onClose={() => setSheet(null)}
      />
    ) : sheet ? (
      <ActionSheet title={`${sheet.id} ${sheet.title}`} groups={groupsFor(sheet)} onClose={() => setSheet(null)} />
    ) : null;

  const buttons = (
    <>
      {p.workspaces.length ? (
        <HeaderButton icon={ExternalLink} label="Linear" showLabel onClick={() => setSheet("workspaces")} />
      ) : null}
      {p.onNewTicket ? <HeaderButton icon={Plus} label="New ticket" showLabel primary onClick={p.onNewTicket} /> : null}
    </>
  );
  const empty = p.loading ? null : <EmptyLine>{p.error ? "Nothing to show." : "Every open ticket has a clone."}</EmptyLine>;

  if (p.column) {
    return (
      <section
        aria-label="Tickets"
        className={`flex h-full w-72 shrink-0 flex-col ${frame}`}
      >
        <h3 className={`flex h-10 shrink-0 items-center gap-1.5 pl-3 pr-1 text-sm font-semibold ${TEXT}`}>
          Tickets
          <span className={`tabular-nums ${TEXT_3}`}>{p.loading ? "…" : p.tickets.length}</span>
          <span className="flex-1" />
          {buttons}
        </h3>
        {p.error ? <p className="px-3 pb-1 text-xs text-rose-600 dark:text-rose-400">{p.error}</p> : null}
        {p.tickets.length === 0 ? (
          empty
        ) : (
          <ul className="min-h-0 flex-1 space-y-1.5 overflow-y-auto overscroll-y-contain px-1.5 pb-1.5">
            {p.tickets.map((t) => (
              <TicketRow
                key={t.id}
                ticket={t}
                card
                open={p.open?.id === t.id}
                onOpen={() => p.onOpen(t)}
                onMenu={() => setSheet(t)}
              />
            ))}
          </ul>
        )}
        {sheetView}
      </section>
    );
  }

  if (p.open) {
    const open = p.open;
    return (
      <div className="relative h-full">
        <Page
          fill
          header={
            <PageHeader title="Tickets" sub={open.id} onBack={p.onBack}>
              <HeaderButton icon={Ellipsis} label={`Actions for ${open.id}`} onClick={() => setSheet(open)} />
            </PageHeader>
          }
        >
          {p.detail}
        </Page>
        {sheetView}
      </div>
    );
  }

  return (
    <div className="relative h-full">
      <Page
        error={p.error}
        header={
          <PageHeader title="Tickets" sub={p.loading ? "Loading…" : `${p.tickets.length} without a clone`}>
            {buttons}
          </PageHeader>
        }
      >
        {p.tickets.length === 0 ? (
          empty
        ) : (
          <ul className={`divide-y ${DIVIDE}`}>
            {p.tickets.map((t) => (
              <TicketRow key={t.id} ticket={t} onOpen={() => p.onOpen(t)} onMenu={() => setSheet(t)} />
            ))}
          </ul>
        )}
      </Page>
      {sheetView}
    </div>
  );
}

/** One ticket in the list: its marks and labels over the identifier and title. */
export function TicketRow({
  ticket,
  card = false,
  open = false,
  onOpen,
  onMenu,
}: {
  ticket: LinearTicket;
  /** A card in the board's column (wide), not a row in a list. */
  card?: boolean;
  /** Its panel is open beside the board. */
  open?: boolean;
  onOpen: () => void;
  onMenu: () => void;
}) {
  return (
    <li
      className={`flex items-stretch ${
        card
          ? `overflow-hidden ${CARD} ${CARD_FILL} ${open ? "!border-neutral-900 dark:!border-white" : ""}`
          : ""
      }`}
    >
      <button type="button" onClick={onOpen} aria-current={open ? "true" : undefined} className={`min-w-0 flex-1 px-3 py-2 text-left ${PRESS}`}>
        <span className="flex flex-wrap items-center gap-1.5">
          <StateIcon state={ticket.state} label={ticket.stateName ?? undefined} />
          {ticket.priority ? <PriorityIcon level={ticket.priority} /> : null}
          {ticket.labels.map((l) => (
            <LabelPill key={l.id || l.name} name={l.name} color={l.color} />
          ))}
        </span>
        <span className={`mt-1 line-clamp-2 block text-sm leading-snug ${TEXT}`}>
          <span
            className={`mr-1 inline-block rounded px-1 py-0.5 align-middle text-[10px] font-semibold leading-none ${workspaceBadge(
              ticket.team ?? ticket.id.split("-")[0],
            )}`}
          >
            {ticket.id}
          </span>
          {ticket.title}
        </span>
      </button>
      <button
        type="button"
        onClick={onMenu}
        aria-label={`Actions for ${ticket.id}`}
        className={`flex w-11 shrink-0 items-center justify-center ${TEXT_2} ${PRESS}`}
      >
        <Ellipsis aria-hidden className="size-4" />
      </button>
    </li>
  );
}
