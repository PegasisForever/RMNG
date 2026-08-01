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
// Linear's own reading order, in one narrow column instead of its two: identifier and state
// on top, then the title, then the properties, the description, and the sub-issues. Linear
// runs the properties down a right-hand rail, which a floating panel has no room for, so
// they become a wrapped row under the title.
//
// Pure, like AppShellV2: the description arrives as a slot, because rendering markdown means
// BlockNote, which is browser-only and lazily loaded.
import { Check, ExternalLink, GitBranch } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import TextareaAutosize from "react-textarea-autosize";

import {
  LabelPill,
  PriorityIcon,
  PRIORITY_LABEL,
  StateIcon,
  STATE_LABEL,
} from "~/components/TicketColumn";
import { copyText } from "~/lib/clipboard";
import { branchNameOf, type LinearTicket } from "~/lib/tickets";
import type { TicketLink } from "~/lib/wire/TicketLink";
import { workspaceBadge } from "~/lib/workspace";

/** One property: a muted term over its value, so a wrapped row still reads in pairs. */
function Property({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-[10px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
        {label}
      </dt>
      <dd className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-slate-700 dark:text-slate-200">
        {children}
      </dd>
    </div>
  );
}

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
      className="shrink-0 rounded p-1 text-slate-400 hover:bg-slate-500/10 hover:text-slate-600 dark:hover:text-slate-300"
    >
      {copied ? (
        <Check aria-hidden className="size-4 text-emerald-600 dark:text-emerald-400" />
      ) : (
        <GitBranch aria-hidden className="size-4" />
      )}
    </button>
  );
}

/** A parent or sub-issue: its state mark, its identifier, and its title. The whole row is a
 *  link out to Linear, since this board has no page of its own to send you to. */
function LinkRow({ link }: { link: TicketLink }) {
  const done = link.state === "done" || link.state === "canceled";
  return (
    <a
      href={link.url}
      target="_blank"
      rel="noopener noreferrer"
      className="flex items-center gap-2 rounded-lg px-2 py-1.5 hover:bg-slate-500/10"
    >
      <StateIcon state={link.state} />
      <span className="shrink-0 font-mono text-[11px] text-slate-500 dark:text-slate-400">
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
      <h2 className="text-base font-semibold leading-snug text-slate-900 dark:text-slate-100">
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
      className="w-full resize-none rounded bg-transparent text-base font-semibold leading-snug text-slate-900 outline-none hover:bg-slate-500/5 focus:bg-slate-500/5 dark:text-slate-100"
    />
  );
}

export function TicketPanel({
  ticket,
  description,
  onCreateClone,
  onTitleChange,
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
        <span className="flex min-w-0 flex-1 items-center gap-1.5 text-[11px] text-slate-500 dark:text-slate-400">
          <StateIcon state={ticket.state} />
          <span className="truncate">{STATE_LABEL[ticket.state]}</span>
        </span>
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
        <EditableTitle ticket={ticket} onTitleChange={onTitleChange} />

        {ticket.parent ? (
          <div>
            <div className="text-[10px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
              Parent
            </div>
            <LinkRow link={ticket.parent} />
          </div>
        ) : null}

        {/* Linear's right-hand property rail, wrapped into a row. Only what is set shows:
            a column of "None" tells you nothing you could act on. */}
        <dl className="flex flex-wrap gap-x-6 gap-y-3">
          {ticket.priority ? (
            <Property label="Priority">
              <PriorityIcon level={ticket.priority} />
              {PRIORITY_LABEL[ticket.priority]}
            </Property>
          ) : null}
          {ticket.assignee ? <Property label="Assignee">{ticket.assignee}</Property> : null}
          {ticket.estimate !== undefined ? (
            <Property label="Estimate">{ticket.estimate}</Property>
          ) : null}
          {ticket.dueDate ? <Property label="Due">{ticket.dueDate}</Property> : null}
          {ticket.labels.length > 0 ? (
            <Property label="Labels">
              <span className="flex flex-wrap items-center gap-1.5">
                {ticket.labels.map((label) => (
                  <LabelPill key={label.name} name={label.name} color={label.color} />
                ))}
              </span>
            </Property>
          ) : null}
        </dl>

        <section>
          <h3 className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
            Description
          </h3>
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
                <LinkRow key={child.id} link={child} />
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
            className="w-full rounded-lg bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-700"
          >
            Create a clone for {ticket.id}
          </button>
        </footer>
      ) : null}
    </div>
  );
}
