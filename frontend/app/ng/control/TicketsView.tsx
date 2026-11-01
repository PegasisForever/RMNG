// The /ng Tickets page: the dashboard's ticket column, as a list, and one ticket at a time in
// the same page. The open Linear tickets no clone was made for, in the operator's order.
//
// What the desktop does with the pointer is a tap here: a tap on a row opens the ticket, the
// ⋯ on a row opens a sheet of everything the card's menu does, and the drag that orders the
// column is Move up / Move down / Move to top in that sheet. A drag onto a clone column
// (which makes a clone) is "Create a clone" in the sheet.
//
// Wide (`column`), the list is the board's first column, with the dashboard's own card per
// ticket (`TicketCardBody`) on the control panel's glass, and the open ticket shows beside the board
// (the container puts `detail` there).
//
// As on the dashboard: a ticket drags within the list, or (wide) onto a clone column, which
// makes a clone for it (`./BoardDnd.tsx`: a mouse at once, a finger after a long press); a
// mouse's click on ⋯ opens the dashboard's menu, a finger's the sheet.
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
import { type MouseEvent, type ReactNode, useState } from "react";

import type { Box } from "~/components/OverflowMenu";
import { LabelPill, PriorityIcon, StateIcon, TicketCardBody } from "~/components/TicketColumn";
import type { LinearWorkspace } from "~/lib/linear/types";
import { branchNameOf, type LinearTicket, TICKET_COLUMN_ID, ticketDragId } from "~/lib/tickets";
import { workspaceBadge } from "~/lib/workspace";

import { DragItem, DropColumn, useBoardDnd } from "./BoardDnd";
import { type Move, movesFor } from "./order";
import {
  ActionSheet,
  boxOf,
  CARD,
  CARD_BOX,
  CARD_LIST,
  DesktopMenu,
  DIVIDE,
  EmptyLine,
  HeaderButton,
  isMouseClick,
  NO_DRAG,
  NO_SCROLLBARS,
  Page,
  PageHeader,
  PRESS,
  type SheetGroup,
  TEXT,
  TEXT_2,
  TEXT_3,
  useColumnFrame,
} from "./ui";

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
  const dnd = useBoardDnd();
  const [sheet, setSheet] = useState<LinearTicket | "workspaces" | null>(null);
  /** The dashboard's menu, opened by a mouse, and where. */
  const [menu, setMenu] = useState<{ of: LinearTicket | "workspaces"; at: Box } | null>(null);
  const openMenu = (of: LinearTicket | "workspaces", e: MouseEvent) =>
    isMouseClick(e) ? setMenu({ of, at: boxOf(e) }) : setSheet(of);
  // While one is in the air, the tickets in the order it would leave them
  const byId = new Map(p.tickets.map((t) => [t.id, t]));
  const tickets = dnd?.ticketOrder
    ? dnd.ticketOrder.flatMap((id) => (byId.has(id) ? [byId.get(id)!] : []))
    : p.tickets;
  const ids = p.tickets.map((t) => t.id);
  const dragIds = tickets.map((t) => ticketDragId(t.id));

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

  const workspaceGroups: SheetGroup[] = [
    { actions: p.workspaces.map((w) => ({ icon: ExternalLink, label: w.name, onSelect: () => p.onOpenWorkspace(w) })) },
  ];
  const sheetView = (
    <>
      {sheet === "workspaces" ? (
        <ActionSheet title="Open a Linear workspace" groups={workspaceGroups} onClose={() => setSheet(null)} />
      ) : sheet ? (
        <ActionSheet title={`${sheet.id} ${sheet.title}`} groups={groupsFor(sheet)} onClose={() => setSheet(null)} />
      ) : null}
      {menu ? (
        <DesktopMenu
          at={menu.at}
          groups={menu.of === "workspaces" ? workspaceGroups : groupsFor(menu.of)}
          onClose={() => setMenu(null)}
        />
      ) : null}
    </>
  );

  /** The dashboard's card for a ticket (wide); the click opens it beside the board. */
  const card = (t: LinearTicket, drag?: Record<string, unknown>) => (
    <div
      {...drag}
      onClick={() => p.onOpen(t)}
      className={`cursor-grab overflow-hidden ${CARD_BOX} ${
        p.open?.id === t.id ? "!border-neutral-900 dark:!border-white" : ""
      }`}
    >
      <TicketCardBody
        ticket={t}
        selected={p.open?.id === t.id}
        onCreateClone={() => p.onCreateClone(t)}
        onOpenInLinear={p.onOpenInLinear}
        onCopyBranchName={(b) => void p.onCopy(b)}
        onCopyTicketLink={(u) => void p.onCopy(u)}
        onCancel={() => p.onCancel(t)}
        onMoveToBacklog={() => p.onMoveToBacklog(t)}
        onMenuTouch={() => setSheet(t)}
      />
    </div>
  );
  const row = (t: LinearTicket, drag?: Record<string, unknown>) => (
    <div {...drag}>
      <TicketRow ticket={t} onOpen={() => p.onOpen(t)} onMenu={(e) => openMenu(t, e)} />
    </div>
  );
  // What follows the pointer while a ticket is dragged
  if (dnd) {
    dnd.overlays.current.ticket = (id) => {
      const t = byId.get(id);
      return t ? (p.column ? card(t) : row(t)) : null;
    };
  }

  const buttons = (
    <>
      {p.workspaces.length ? (
        <HeaderButton icon={ExternalLink} label="Linear" showLabel onClick={(e) => openMenu("workspaces", e)} />
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
        <DropColumn
          id={TICKET_COLUMN_ID}
          items={dragIds}
          className={`min-h-0 flex-1 overflow-y-auto overscroll-y-contain ${CARD_LIST}`}
        >
          {tickets.length === 0
            ? empty
            : tickets.map((t) => (
                <DragItem key={t.id} id={ticketDragId(t.id)}>
                  {(drag) => card(t, drag)}
                </DragItem>
              ))}
        </DropColumn>
        {sheetView}
      </section>
    );
  }

  if (p.open) {
    const open = p.open;
    return (
      // The ticket scrolls with no bar
      <div className={`relative h-full ${NO_SCROLLBARS}`}>
        <Page
          fill
          header={
            <PageHeader title="Tickets" sub={open.id} onBack={p.onBack}>
              <HeaderButton icon={Ellipsis} label={`Actions for ${open.id}`} onClick={(e) => openMenu(open, e)} />
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
        {tickets.length === 0 ? (
          empty
        ) : (
          <DropColumn id={TICKET_COLUMN_ID} items={dragIds} className={`divide-y ${DIVIDE}`}>
            {tickets.map((t) => (
              <DragItem key={t.id} id={ticketDragId(t.id)}>
                {(drag) => row(t, drag)}
              </DragItem>
            ))}
          </DropColumn>
        )}
      </Page>
      {sheetView}
    </div>
  );
}

/** One ticket in the list: its marks and labels over the identifier and title. */
export function TicketRow({
  ticket,
  onOpen,
  onMenu,
}: {
  ticket: LinearTicket;
  onOpen: () => void;
  onMenu: (e: MouseEvent<HTMLButtonElement>) => void;
}) {
  return (
    <div className="flex items-stretch">
      <button type="button" onClick={onOpen} className={`min-w-0 flex-1 px-3 py-2 text-left ${PRESS}`}>
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
        {...NO_DRAG}
        onClick={onMenu}
        aria-label={`Actions for ${ticket.id}`}
        className={`flex w-11 shrink-0 items-center justify-center ${TEXT_2} ${PRESS}`}
      >
        <Ellipsis aria-hidden className="size-4" />
      </button>
    </div>
  );
}
