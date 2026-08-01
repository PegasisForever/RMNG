// The selected Linear ticket, filling the side panel on its own.
//
// It replaces the notes and chat cards rather than joining them, because a ticket is not a
// third thing about the selected clone: it is a different subject entirely, and stacking it
// with two clone-scoped cards would suggest otherwise.
//
// No close button. Picking any clone closes it and puts that clone's two cards back, which
// is the same click the operator makes next anyway — a dedicated X would be a second way to
// do the thing they are already doing.
//
// Linear's own reading order, in one narrow column instead of its two: the identifier and
// the ticket's marks on top, then the title, the parent, the description, and the
// sub-issues. The marks are the card's own row, unchanged: state, priority, labels. A card
// and its panel are the same ticket, so reading one teaches you the other.
//
// Pure, like AppShellV2: the description arrives as a slot, because rendering markdown means
// BlockNote, which is browser-only and lazily loaded.
import { Check, ExternalLink, GitBranch } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import TextareaAutosize from "react-textarea-autosize";

import { LabelPill, PriorityIcon, StateIcon } from "~/components/TicketColumn";
import { copyText } from "~/lib/clipboard";
import { branchNameOf, type LinearTicket } from "~/lib/tickets";
import type { TicketLink } from "~/lib/wire/TicketLink";
import { workspaceBadge } from "~/lib/workspace";

/** Copy the ticket's git branch name. It sits in the header rather than among the
 *  properties because it is the one property nobody reads — they copy it and paste it into
 *  a terminal, so the button is the whole point and the string is just its tooltip. */
function CopyBranch({ ticket }: { ticket: LinearTicket }) {
  const [copied, setCopied] = useState(false);
  const branch = branchNameOf(ticket);
  return (
    <button
      type="button"
      onClick={() => {
        void copyText(branch).then((ok) => {
          if (!ok) return;
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1200);
        });
      }}
      title={`Copy branch name: ${branch}`}
      aria-label="Copy branch name"
      className="shrink-0 cursor-pointer rounded p-1 text-slate-400 hover:bg-slate-500/10 hover:text-slate-600 dark:hover:text-slate-300"
    >
      {copied ? (
        <Check aria-hidden className="size-4 text-emerald-600 dark:text-emerald-400" />
      ) : (
        <GitBranch aria-hidden className="size-4" />
      )}
    </button>
  );
}

/** What a referenced issue turns into on this board. */
export interface TicketLinkTarget {
  /** The row's tooltip. It names where the click lands, since the two kinds of row look
   *  alike and only one of them leaves the app. */
  title: string;
  /** Show it: select the ticket, or activate the clone somebody made for it. */
  open: () => void;
}

/** A parent or sub-issue: its state mark, its identifier, and its title.
 *
 *  Where the row goes depends on whether the referenced issue is on this board. One that is
 *  becomes a button that selects it here, because sending the operator to Linear for a thing
 *  sitting one column over is a worse answer to the same click. One that is not stays a link
 *  out, and the arrow at its end is what says so before the click rather than after. */
function LinkRow({ link, here }: { link: TicketLink; here: TicketLinkTarget | null }) {
  const done = link.state === "done" || link.state === "canceled";
  // `flex-1` and `min-w-0` are for the parent row, where the row shares a line with its term
  // and has to give way to it. In the sub-issue list they do nothing, the container there
  // being a block.
  const row =
    "flex w-full min-w-0 flex-1 cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 text-left hover:bg-slate-500/10";
  const body = (
    <>
      <StateIcon state={link.state} />
      {/* Nudged down a pixel. Flex centres the two spans' line boxes, which puts their
          baselines level, and level baselines are what makes this one look high: an
          identifier is all caps and digits with nothing below the baseline, while the title
          next to it runs from cap height down through its descenders. The pixel puts the two
          blocks of ink on the same middle. */}
      <span className="shrink-0 translate-y-px font-mono text-[11px] text-slate-500 dark:text-slate-400">
        {link.id}
      </span>
      <span
        className={`min-w-0 flex-1 truncate text-xs ${
          done
            ? "text-slate-400 line-through dark:text-slate-500"
            : "text-slate-700 dark:text-slate-200"
        }`}
      >
        {link.title}
      </span>
    </>
  );

  if (here) {
    return (
      <button type="button" onClick={here.open} title={here.title} className={row}>
        {body}
      </button>
    );
  }
  return (
    <a
      href={link.url}
      target="_blank"
      rel="noopener noreferrer"
      title={`Open ${link.id} in Linear`}
      className={row}
    >
      {body}
      <ExternalLink aria-hidden className="size-3 shrink-0 text-slate-300 dark:text-slate-600" />
    </a>
  );
}

export interface TicketPanelProps {
  ticket: LinearTicket;
  /** The rendered description. Absent ⇒ the ticket has no body, and the panel says so
   *  rather than leaving a hole where one would be. */
  description?: ReactNode;
  /** Start a clone on this ticket. Absent ⇒ the button is not offered. */
  onCreateClone?: () => void;
  /** Persist a new title to Linear. Absent ⇒ the title is read-only. */
  onTitleChange?: (title: string) => void;
  /** Where the parent and sub-issue rows go. Called per row with the referenced identifier.
   *  Return null when that issue is not on this board and the row should open Linear.
   *  Absent ⇒ every row opens Linear, which is what a panel with no board behind it wants. */
  resolveLink?: (ticketId: string) => TicketLinkTarget | null;
}

/** The title, editable in place.
 *
 *  A textarea rather than an input, because a Linear title wraps and an input would scroll
 *  it sideways under a cursor. It commits on blur and on Enter, and Escape puts back what
 *  was there: the same three keys every rename in this app answers to.
 *
 *  Held locally while it is being typed and reseeded when the ticket's own title changes,
 *  which is what lets somebody else's edit in Linear land here without fighting a cursor. */
function EditableTitle({
  ticket,
  onTitleChange,
}: {
  ticket: LinearTicket;
  onTitleChange?: (title: string) => void;
}) {
  const [draft, setDraft] = useState(ticket.title);
  useEffect(() => setDraft(ticket.title), [ticket.id, ticket.title]);

  const commit = () => {
    const next = draft.trim();
    if (!next || next === ticket.title) {
      setDraft(ticket.title);
      return;
    }
    onTitleChange?.(next);
  };

  if (!onTitleChange) {
    return (
      <h2 className="text-xl font-semibold leading-snug text-slate-900 dark:text-slate-100">
        {ticket.title}
      </h2>
    );
  }
  return (
    <TextareaAutosize
      value={draft}
      aria-label="Ticket title"
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          e.currentTarget.blur();
        }
        if (e.key === "Escape") {
          setDraft(ticket.title);
          e.currentTarget.blur();
        }
      }}
      className="w-full resize-none rounded bg-transparent text-xl font-semibold leading-snug text-slate-900 outline-none hover:bg-slate-500/5 focus:bg-slate-500/5 dark:text-slate-100"
    />
  );
}

export function TicketPanel({
  ticket,
  description,
  onCreateClone,
  onTitleChange,
  resolveLink,
}: TicketPanelProps) {
  const done = ticket.children.filter(
    (c) => c.state === "done" || c.state === "canceled",
  ).length;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex shrink-0 items-center gap-2 px-4 pt-3">
        <span
          className={`shrink-0 rounded px-1 py-0.5 text-[10px] font-semibold leading-none ${workspaceBadge(
            ticket.team ?? ticket.id.split("-")[0],
          )}`}
        >
          {ticket.id}
        </span>
        {/* The card's own row of marks, the same three in the same order. No state or
            priority word next to them: a panel this narrow has better uses for the width,
            and both marks name themselves on hover. A label already reads as its own name. */}
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
          <StateIcon state={ticket.state} />
          {ticket.priority ? <PriorityIcon level={ticket.priority} /> : null}
          {ticket.labels.map((label) => (
            <LabelPill key={label.name} name={label.name} color={label.color} />
          ))}
        </div>
        <CopyBranch ticket={ticket} />
        <a
          href={ticket.url}
          target="_blank"
          rel="noopener noreferrer"
          title="Open in Linear"
          aria-label="Open in Linear"
          className="shrink-0 rounded p-1 text-slate-400 hover:bg-slate-500/10 hover:text-slate-600 dark:hover:text-slate-300"
        >
          <ExternalLink aria-hidden className="size-4" />
        </a>
      </header>

      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 pb-4 pt-2">
        {/* The title and its parent are one unit, held closer to each other than to anything
            else. The parent names the thing the title is part of, so a section-sized gap
            between them would read as two subjects instead of one. */}
        <div className="space-y-0.5">
          <EditableTitle ticket={ticket} onTitleChange={onTitleChange} />

          {/* The term rides the row rather than sitting above it. A ticket has one parent, so
              a heading over a single item spends a line saying what the row beneath it
              already says, and the panel is narrow enough that lines are the scarce thing. */}
          {ticket.parent ? (
            <div className="-mr-2 flex items-center gap-2">
              <span className="shrink-0 text-[10px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
                Parent
              </span>
              <LinkRow link={ticket.parent} here={resolveLink?.(ticket.parent.id) ?? null} />
            </div>
          ) : null}
        </div>

        {/* No heading over the body. It follows the title, which is the only thing it could
            be about, and the editor's own first line says the rest. */}
        <section>
          {description ?? (
            <p className="text-xs text-slate-400 dark:text-slate-500">No description.</p>
          )}
        </section>

        {ticket.children.length > 0 ? (
          <section>
            <h3 className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
              Sub-issues {done}/{ticket.children.length}
            </h3>
            <div className="-mx-2">
              {ticket.children.map((child) => (
                <LinkRow key={child.id} link={child} here={resolveLink?.(child.id) ?? null} />
              ))}
            </div>
          </section>
        ) : null}
      </div>

      {onCreateClone ? (
        <footer className="shrink-0 border-t border-slate-900/10 px-4 py-2 dark:border-white/10">
          <button
            type="button"
            onClick={onCreateClone}
            className="w-full cursor-pointer rounded-lg bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-700"
          >
            Create a clone for {ticket.id}
          </button>
        </footer>
      ) : null}
    </div>
  );
}
