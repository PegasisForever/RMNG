// A Linear ticket as a card: its state, priority and labels in Linear's own marks, its id and
// title, and its ⋮ menu. The marks are shared with the ticket panel and the new-ticket dialog.
import { CircleDashed, CircleX, ExternalLink, GitBranch, Link2, Plus, X } from "lucide-react";

import { MenuDivider, MenuItem, OverflowMenu } from "~/components/OverflowMenu";
import { CARD_FILL_ON_GLASS } from "~/desk/control/ui";
import { branchNameOf, type LinearTicket } from "~/lib/tickets";
import { workspaceBadge } from "~/lib/workspace";

/** What each kind of state is called when the workspace's own name for it is not to hand.
 *
 *  Where it is, that name wins: the state menu lists a team's whole workflow by its real
 *  names, and this is only what a card's tooltip and that menu's fallback read. */
export const STATE_LABEL: Record<LinearTicket["state"], string> = {
  triage: "Triage",
  backlog: "Backlog",
  todo: "Todo",
  in_progress: "In progress",
  done: "Done",
  canceled: "Cancelled",
  duplicate: "Duplicate",
};

const STATE_COLOR: Record<LinearTicket["state"], string> = {
  triage: "text-orange-500",
  backlog: "text-slate-400 dark:text-slate-500",
  todo: "text-slate-400 dark:text-slate-500",
  in_progress: "text-amber-500",
  done: "text-indigo-500",
  canceled: "text-slate-400 dark:text-slate-500",
  duplicate: "text-slate-400 dark:text-slate-500",
};

/** Linear's state mark, redrawn: a ring that fills as the work moves. Triage carries a dot,
 *  Backlog is dashed, Todo is the plain empty ring, In progress is half filled, Done is
 *  filled with a tick, and Cancelled and Duplicate are struck through. The same glyphs Linear
 *  puts on its own column headers, and Duplicate reads as Cancelled there too.
 *
 *  A ring rather than the clone cards' solid dot. A clone's dot reports something changing
 *  while you watch it, and this reports a state somebody set in another tool, so drawing
 *  them the same way would claim they are the same kind of fact.
 *
 *  Our colour per kind, never Linear's own per state. Two states of one kind are told apart
 *  by their names in the one place both appear, and a workspace is free to give a state a
 *  near-white colour that would leave the glyph invisible on the card. */
export function StateIcon({
  state,
  label = STATE_LABEL[state],
}: {
  state: LinearTicket["state"];
  /** What it announces and shows on hover. Pass the workspace's own state name where it is
   *  known; the kind's name is what it falls back to. */
  label?: string;
}) {
  const struck = state === "canceled" || state === "duplicate";
  return (
    <svg
      viewBox="0 0 14 14"
      role="img"
      aria-label={label}
      className={`size-3.5 shrink-0 ${STATE_COLOR[state]}`}
    >
      <title>{label}</title>
      <circle
        cx="7"
        cy="7"
        r="5.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeDasharray={state === "backlog" ? "2.5 2" : undefined}
      />
      {state === "in_progress" ? (
        <path d="M7 3.5 A3.5 3.5 0 0 1 7 10.5 Z" fill="currentColor" />
      ) : null}
      {state === "done" ? (
        <>
          <circle cx="7" cy="7" r="5.5" fill="currentColor" />
          <path
            d="M4.5 7.2 L6.3 9 L9.6 5.4"
            fill="none"
            stroke="white"
            strokeWidth="1.6"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </>
      ) : null}
      {state === "triage" ? <circle cx="7" cy="7" r="2" fill="currentColor" /> : null}
      {struck ? (
        <path d="M4.6 4.6 L9.4 9.4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      ) : null}
    </svg>
  );
}

/** Linear's priority scale, 1 (urgent) to 4 (low), and the 0 it sends for an issue nobody has
 *  ranked. A card leaves 0 undrawn; the new-ticket dialog names it, because "no priority" is
 *  one of the choices there. */
export const PRIORITY_LABEL: Record<number, string> = {
  0: "No priority",
  1: "Urgent",
  2: "High",
  3: "Medium",
  4: "Low",
};

/** Linear's priority glyph, redrawn: three ascending bars filled up to the level, and a
 *  filled mark for Urgent because urgent is not the top of the scale so much as a different
 *  claim about the work.
 *
 *  It is the glyph and not a worded pill because that is what the operator already reads in
 *  Linear, and because four coloured pills down a column is four colours competing with the
 *  labels, which carry the operator's own scheme and should win. */
export function PriorityIcon({ level }: { level: number }) {
  const label = PRIORITY_LABEL[level];
  if (level === 1) {
    return (
      <svg viewBox="0 0 16 16" className="size-3.5 shrink-0" role="img" aria-label={label}>
        <title>{label}</title>
        <rect x="1" y="1" width="14" height="14" rx="3.5" className="fill-orange-500" />
        <rect x="7" y="4" width="2" height="5" rx="1" className="fill-white" />
        <rect x="7" y="10.5" width="2" height="2" rx="1" className="fill-white" />
      </svg>
    );
  }
  // 2 High fills all three bars, 3 Medium two, 4 Low one, and 0 none. The rest stay faint
  // rather than disappearing, so every glyph is the same shape and only its fill differs.
  const filled = level === 2 ? 3 : level === 3 ? 2 : level === 0 ? 0 : 1;
  const bars = [
    { x: 1.5, y: 9.5, h: 5 },
    { x: 6.5, y: 6.5, h: 8 },
    { x: 11.5, y: 3.5, h: 11 },
  ];
  return (
    <svg viewBox="0 0 16 16" className="size-3.5 shrink-0" role="img" aria-label={label}>
      <title>{label}</title>
      {bars.map((bar, i) => (
        <rect
          key={bar.x}
          x={bar.x}
          y={bar.y}
          width="3"
          height={bar.h}
          rx="1"
          className={i < filled ? "fill-slate-500 dark:fill-slate-300" : "fill-slate-300 dark:fill-slate-600"}
        />
      ))}
    </svg>
  );
}

/** A Linear label: an outlined pill with the label's own colour as a dot. The colour is a
 *  `#rrggbb` string from Linear, so it rides an inline style; everything else is the same
 *  hairline the cards wear.
 *
 *  `onRemove` turns the dot into an X under the pointer. In its place rather than after it,
 *  which is what Linear does and what keeps the pill exactly the width it was: a row of these
 *  wraps, and one growing under the pointer would reflow the rest of the line. The dot is the
 *  label's decoration and the X is the thing you want from it, so the swap costs nothing you
 *  were reading.
 *
 *  Where there is no hover the X is simply always there, which is a phone and anything else the
 *  browser reports `hover: none` for. Tailwind gates `group-hover` behind `@media (hover: hover)`,
 *  so that query is the exact complement of the swap rather than a guess at which devices lack a
 *  pointer: wherever the hover rule cannot fire, this one has already made the swap. It costs a
 *  touch screen the colour, which is decoration, rather than the only way to take a label off. */
export function LabelPill({
  name,
  color,
  onRemove,
}: {
  name: string;
  color: string;
  /** Take the label off the ticket. Absent ⇒ the dot stays a dot, which is what every card
   *  gets. */
  onRemove?: () => void;
}) {
  // Both marks sit in one 10px slot, so which of them is showing never moves the text beside
  // it. The dot keeps its own 8px: an X drawn at the dot's size is a smudge.
  const swap = onRemove
    ? "group-hover:opacity-0 group-focus-within:opacity-0 [@media(hover:none)]:opacity-0"
    : "";
  return (
    <span className="group inline-flex min-w-0 shrink items-center gap-1 rounded-full border border-slate-300 px-1.5 py-0.5 text-[11px] leading-none text-slate-600 dark:border-slate-600 dark:text-slate-300">
      <span className="relative flex size-2.5 shrink-0 items-center justify-center">
        <span
          aria-hidden
          className={`size-2 rounded-full ${swap}`}
          style={{ backgroundColor: color }}
        />
        {onRemove ? (
          <button
            type="button"
            onClick={onRemove}
            title={`Remove ${name}`}
            aria-label={`Remove label ${name}`}
            // `after:-inset-1` grows the hit area past the glyph without giving the button any
            // size of its own, so a 10px target is comfortable to hit and the pill stays put.
            className="absolute inset-0 flex cursor-pointer items-center justify-center rounded-full text-slate-500 opacity-0 after:absolute after:-inset-1 after:content-[''] hover:text-slate-800 group-hover:opacity-100 group-focus-within:opacity-100 [@media(hover:none)]:opacity-100 dark:text-slate-400 dark:hover:text-slate-100"
          >
            <X aria-hidden className="size-2.5" />
          </button>
        ) : null}
      </span>
      <span className="truncate">{name}</span>
    </span>
  );
}

/** The team key that colours the badge. Derived from the id when Linear did not say, so the
 *  badge can never disagree with the identifier printed inside it. */
function teamOf(ticket: LinearTicket): string {
  return ticket.team ?? ticket.id.split("-")[0];
}

/** The ticket's ⋮ menu, on the same shell the clone cards use.
 *
 *  Three kinds of item, in that order: one that starts work here, shortcuts out to Linear,
 *  and two that write a new state back to it. Both writes mean the same thing to the tickets'
 *  column — a ticket that is neither Todo nor In progress is not waiting to be started, so it
 *  leaves the list.
 *
 *  Leaving for Linear and writing to the clipboard both happen to the browser rather than to
 *  the ticket, so the menu asks for them and the container does them. Each item is offered
 *  only when its callback is, the same rule "Create a clone…" already follows. */
function TicketMenu({
  ticket,
  onCreateClone,
  onOpenInLinear,
  onCopyBranchName,
  onCopyTicketLink,
  onCancel,
  onMoveToBacklog,
  onTouch,
}: {
  ticket: LinearTicket;
  onCreateClone?: () => void;
  onOpenInLinear?: (url: string) => void;
  onCopyBranchName?: (branch: string) => void;
  onCopyTicketLink?: (url: string) => void;
  onCancel?: () => void;
  onMoveToBacklog?: () => void;
  onTouch?: () => void;
}) {
  return (
    <OverflowMenu label={`actions for ${ticket.id}`} onTouch={onTouch}>
      {onCreateClone ? (
        <MenuItem icon={Plus} label="Create a clone…" onClick={onCreateClone} />
      ) : null}
      {onOpenInLinear ? (
        <MenuItem
          icon={ExternalLink}
          label="Open in Linear"
          onClick={() => onOpenInLinear(ticket.url)}
        />
      ) : null}
      {onOpenInLinear && (onCopyBranchName || onCopyTicketLink) ? <MenuDivider /> : null}
      {onCopyBranchName ? (
        <MenuItem
          icon={GitBranch}
          label="Copy branch name"
          onClick={() => onCopyBranchName(branchNameOf(ticket))}
        />
      ) : null}
      {onCopyTicketLink ? (
        <MenuItem icon={Link2} label="Copy link" onClick={() => onCopyTicketLink(ticket.url)} />
      ) : null}
      {onMoveToBacklog || onCancel ? <MenuDivider /> : null}
      {onMoveToBacklog ? (
        <MenuItem icon={CircleDashed} label="Move to backlog" onClick={onMoveToBacklog} />
      ) : null}
      {onCancel ? (
        <MenuItem icon={CircleX} label="Cancel ticket" onClick={onCancel} danger />
      ) : null}
    </OverflowMenu>
  );
}

/** The card's own markup, without the drag wiring, so the drag overlay can render the same
 *  thing under the pointer.
 *
 *  Laid out as a clone card is: a muted metadata row up top, then the title line carrying
 *  the `WE-301` badge inline with the title, so a wrapped title flows back to the card's
 *  left edge instead of indenting under the badge. The badge takes its colour from the same
 *  `workspaceBadge` the clone cards use, so a ticket and the clone made from it are the
 *  same colour side by side.
 *
 *  No lifecycle dot and no state word. Linear's own ring says it in one glyph, and a clone
 *  card's solid dot is reserved for something that changes while you watch it rather than a
 *  state somebody set in another tool. */
export function TicketCardBody({
  ticket,
  selected = false,
  onCreateClone,
  onOpenInLinear,
  onCopyBranchName,
  onCopyTicketLink,
  onCancel,
  onMoveToBacklog,
  onMenuTouch,
}: {
  ticket: LinearTicket;
  /** Selected: the panel on the right is showing this ticket. The tint alone says so. No
   *  left accent bar — a card is one object, and a bar down its edge reads as a second one
   *  next to it. */
  selected?: boolean;
  /** Opens the clone dialog for this ticket. Absent ⇒ the item is not offered. */
  onCreateClone?: () => void;
  /** Show the ticket in Linear, which leaves the app. Absent ⇒ the item is not offered. */
  onOpenInLinear?: (url: string) => void;
  /** Put the ticket's git branch name on the clipboard. Absent ⇒ the item is not offered. */
  onCopyBranchName?: (branch: string) => void;
  /** Put the ticket's Linear URL on the clipboard. Absent ⇒ the item is not offered. */
  onCopyTicketLink?: (url: string) => void;
  onCancel?: () => void;
  onMoveToBacklog?: () => void;
  /** A tap with a finger on the ⋮ calls this in place of the dropdown (`OverflowMenu`). */
  onMenuTouch?: () => void;
}) {
  const labels = ticket.labels ?? [];
  return (
    // The fills a clone's card has (`CloneCard`), so the two read as one kind of selection:
    // the tint alone.
    <article
      className={`px-2.5 py-2 ${selected ? CARD_FILL_ON_GLASS.selected : CARD_FILL_ON_GLASS.idle}`}
    >
      {/* State, priority, then labels: the row Linear itself draws, in its own marks. No
          owner on it: every ticket here belongs to one of the operator's own API keys, so
          naming them would repeat the same handful of names down the column and answer a
          question nobody asked. */}
      <div className="flex items-start gap-1.5">
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
          <StateIcon state={ticket.state} />
          {ticket.priority ? <PriorityIcon level={ticket.priority} /> : null}
          {labels.map((label) => (
            <LabelPill key={label.name} name={label.name} color={label.color} />
          ))}
        </div>
        {/* The ⋮ button is a 16px icon in a 24px hit area, ten pixels taller than the marks
            it sits beside. Left alone it sets the row's height and opens a gap under the
            marks that reads as the card's own spacing. Pulling its padding out of the flow
            hands the height back to the marks and centres the icon on them. */}
        <div className="-my-1 shrink-0">
          <TicketMenu
            ticket={ticket}
            onCreateClone={onCreateClone}
            onOpenInLinear={onOpenInLinear}
            onCopyBranchName={onCopyBranchName}
            onCopyTicketLink={onCopyTicketLink}
            onCancel={onCancel}
            onMoveToBacklog={onMoveToBacklog}
            onTouch={onMenuTouch}
          />
        </div>
      </div>

      <p className="mt-1 break-words text-sm font-medium leading-snug text-slate-800 dark:text-slate-100">
        <span
          className={`mr-1 inline-block rounded px-1 py-0.5 align-middle text-[10px] font-semibold leading-none ${workspaceBadge(
            teamOf(ticket),
          )}`}
        >
          {ticket.id}
        </span>
        {ticket.title}
      </p>
    </article>
  );
}
