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
// sub-issues. The marks are the card's own row: state, priority, labels. A card and its panel
// are the same ticket, so reading one teaches you the other.
//
// Two of those marks are also where the ticket is edited. The state ring opens the team's own
// workflow by name, and a label pill carries an X with a "+" after the row. Both write to
// Linear rather than to anything of ours, and the card in the column deliberately gets neither:
// a queue is for scanning, and a click that changes a ticket belongs where it is the subject.
//
// Pure, like AppShellV2: the description arrives as a slot, because rendering markdown means
// BlockNote, which is browser-only and lazily loaded.
import { Check, ExternalLink, GitBranch, Plus } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import TextareaAutosize from "react-textarea-autosize";

import { MenuChoice, MenuNote, OverflowMenu } from "~/components/OverflowMenu";
import { LabelPill, PriorityIcon, StateIcon, STATE_LABEL } from "~/components/TicketColumn";
import { branchNameOf, type LinearTicket } from "~/lib/tickets";
import type { TicketLabel, TicketLink, TicketWorkflowState } from "~/lib/linear/types";
import { workspaceBadge } from "~/lib/workspace";

/** Copy the ticket's git branch name. It sits in the header rather than among the
 *  properties because it is the one property nobody reads — they copy it and paste it into
 *  a terminal, so the button is the whole point and the string is just its tooltip.
 *
 *  The clipboard write belongs to the container; the tick that follows it does not. Whether
 *  the button is currently showing a tick is this button's own business and nothing else's,
 *  so it stays here with the timer that clears it. Only a write that actually landed turns
 *  it on, which is why the callback answers with whether it did. */
function CopyBranch({
  ticket,
  onCopyBranchName,
}: {
  ticket: LinearTicket;
  onCopyBranchName: (branch: string) => Promise<boolean>;
}) {
  const [copied, setCopied] = useState(false);
  const branch = branchNameOf(ticket);
  return (
    <button
      type="button"
      onClick={() => {
        void onCopyBranchName(branch).then((ok) => {
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

/** The state mark, as a menu that changes it.
 *
 *  Clicking the glyph is how Linear does it, and it is the only affordance the header has room
 *  for: a worded dropdown would take the width the labels need. Each row carries the same ring
 *  the header does, so the menu teaches the glyph.
 *
 *  The rows are the team's own states in the team's own order, so a workspace that calls its
 *  backlog "Icebox" and keeps an "In Review" beside "In Progress" gets both, spelled its way.
 *  Two states of one kind wear the same ring, and their names are what tell them apart. */
function StateMenu({
  ticket,
  states,
  loading,
  onStateChange,
}: {
  ticket: LinearTicket;
  states: TicketWorkflowState[];
  loading: boolean;
  onStateChange: (state: TicketWorkflowState) => void;
}) {
  // The state's own name where the ticket carries it, and the kind's otherwise. A ticket that
  // reached the panel without one is a fixture or an older answer, not a ticket in no state.
  const current = ticket.stateName ?? STATE_LABEL[ticket.state];
  return (
    <OverflowMenu
      label={`Change the state of ${ticket.id}, currently ${current}`}
      align="left"
      trigger={(open) => (
        <span className={`rounded p-0.5 ${open ? "bg-slate-500/15" : "hover:bg-slate-500/15"}`}>
          <StateIcon state={ticket.state} label={current} />
        </span>
      )}
    >
      {loading ? <MenuNote>Loading states…</MenuNote> : null}
      {!loading && states.length === 0 ? <MenuNote>No workflow to read.</MenuNote> : null}
      <div className="max-h-64 overflow-y-auto">
        {states.map((state) => (
          <MenuChoice
            key={state.id}
            icon={<StateIcon state={state.type} label={state.name} />}
            label={state.name}
            // By id, so two states of one kind are two rows and only the ticket's own is
            // ticked. A ticket with no state id falls back to matching on the kind, which is
            // right for the one state of that kind and harmless where there are two.
            selected={
              ticket.stateId ? state.id === ticket.stateId : state.type === ticket.state
            }
            onClick={() => onStateChange(state)}
          />
        ))}
      </div>
    </OverflowMenu>
  );
}

/** The "+" after the labels, and the menu of what can go on.
 *
 *  It offers what the ticket does not already carry, so the list is what a click would change
 *  rather than a checklist of everything the team has. A label already on the ticket comes off
 *  through its own pill, which is where the operator is already pointing.
 *
 *  Three different empty answers, because they mean three different things: still asking, the
 *  team has no labels, and every label is already on this ticket. */
function AddLabelMenu({
  ticket,
  options,
  loading,
  onAddLabel,
}: {
  ticket: LinearTicket;
  options: TicketLabel[];
  loading: boolean;
  onAddLabel: (label: TicketLabel) => void;
}) {
  const on = new Set(ticket.labels.map((l) => l.id));
  const available = options.filter((label) => !on.has(label.id));
  return (
    <OverflowMenu
      label={`Add a label to ${ticket.id}`}
      trigger={(open) => (
        <span
          className={`inline-flex items-center rounded-full border border-dashed px-1.5 py-0.5 leading-none ${
            open
              ? "border-slate-400 text-slate-600 dark:border-slate-500 dark:text-slate-300"
              : "border-slate-300 text-slate-400 hover:border-slate-400 hover:text-slate-600 dark:border-slate-600 dark:hover:border-slate-500 dark:hover:text-slate-300"
          }`}
        >
          <Plus aria-hidden className="size-3" />
        </span>
      )}
    >
      {loading ? <MenuNote>Loading labels…</MenuNote> : null}
      {!loading && available.length === 0 ? (
        <MenuNote>
          {options.length === 0 ? "No labels in this team." : "Every label is already on."}
        </MenuNote>
      ) : null}
      {/* Capped and scrolled, because this is the one menu whose length is somebody else's
          decision: a workspace with sixty labels would otherwise run off the bottom of the
          screen with no way to reach the end of it. */}
      <div className="max-h-64 overflow-y-auto">
        {available.map((label) => (
          <MenuChoice
            key={label.id}
            icon={
              <span
                aria-hidden
                className="size-2.5 rounded-full"
                style={{ backgroundColor: label.color }}
              />
            }
            label={label.name}
            onClick={() => onAddLabel(label)}
          />
        ))}
      </div>
    </OverflowMenu>
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
  // Struck through once the work is over, however it ended: finished, dropped, or folded
  // into another issue.
  const done =
    link.state === "done" || link.state === "canceled" || link.state === "duplicate";
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
  /** Put the ticket's git branch name on the clipboard, answering with whether it landed.
   *  Writing there is a browser API and its two paths can both refuse, so the container owns
   *  the call and the header shows its tick only on a real success. */
  onCopyBranchName: (branch: string) => Promise<boolean>;
  /** Start a clone on this ticket. Absent ⇒ the button is not offered. */
  onCreateClone?: () => void;
  /** Persist a new title to Linear. Absent ⇒ the title is read-only. */
  onTitleChange?: (title: string) => void;
  /** Move the ticket into another workflow state. Absent ⇒ the state mark is a glyph rather
   *  than a menu, which is what a panel with no key behind it gets. */
  onStateChange?: (state: TicketWorkflowState) => void;
  /** The team's workflow, in its own order, for the state menu. */
  stateOptions?: TicketWorkflowState[];
  /** The workflow lookup is still in flight. The menu says so instead of looking empty. */
  statesLoading?: boolean;
  /** Every label the ticket's team can carry, for the "+" menu. The ones already on the ticket
   *  are filtered out of it here rather than by the caller. */
  labelOptions?: TicketLabel[];
  /** The label lookup is still in flight. The menu says so instead of claiming the team has
   *  none. */
  labelsLoading?: boolean;
  /** Put a label on the ticket. Absent ⇒ no "+" button. */
  onAddLabel?: (label: TicketLabel) => void;
  /** Take a label off the ticket. Absent ⇒ the pills carry no X. A label that reached the
   *  browser with no id is left alone either way: a write has nothing to name. */
  onRemoveLabel?: (label: TicketLabel) => void;
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
  onCopyBranchName,
  onCreateClone,
  onTitleChange,
  onStateChange,
  stateOptions = [],
  statesLoading = false,
  labelOptions = [],
  labelsLoading = false,
  onAddLabel,
  onRemoveLabel,
  resolveLink,
}: TicketPanelProps) {
  const done = ticket.children.filter(
    (c) => c.state === "done" || c.state === "canceled" || c.state === "duplicate",
  ).length;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* `px-6` down the card, wider than the app's usual `px-4`. Everything here is prose the
          operator reads rather than chrome they scan, and prose wants a margin: the ticket's
          own text should not start where a card's border ends. */}
      <header className="flex shrink-0 items-center gap-2 px-6 pt-3">
        {/* The card's own row of marks, the same three in the same order. No state or
            priority word next to them: a panel this narrow has better uses for the width,
            and both marks name themselves on hover. A label already reads as its own name.
            Two of the three are editable here and none of them is on a card, because this is
            the one place the ticket is the subject rather than one row of a queue. */}
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
          {/* The identifier rides the same wrapping row rather than sitting outside it. A
              ticket with enough labels wraps to three lines, and a badge held out of the flow
              is centred against all three, which puts it beside the second one. */}
          <span
            className={`shrink-0 rounded px-1 py-0.5 text-[10px] font-semibold leading-none ${workspaceBadge(
              ticket.team ?? ticket.id.split("-")[0],
            )}`}
          >
            {ticket.id}
          </span>
          {onStateChange ? (
            <StateMenu
              ticket={ticket}
              states={stateOptions}
              loading={statesLoading}
              onStateChange={onStateChange}
            />
          ) : (
            <StateIcon state={ticket.state} label={ticket.stateName ?? undefined} />
          )}
          {ticket.priority ? <PriorityIcon level={ticket.priority} /> : null}
          {ticket.labels.map((label) => (
            <LabelPill
              key={label.id || label.name}
              name={label.name}
              color={label.color}
              onRemove={
                onRemoveLabel && label.id !== "" ? () => onRemoveLabel(label) : undefined
              }
            />
          ))}
          {onAddLabel ? (
            <AddLabelMenu
              ticket={ticket}
              options={labelOptions}
              loading={labelsLoading}
              onAddLabel={onAddLabel}
            />
          ) : null}
        </div>
        <CopyBranch ticket={ticket} onCopyBranchName={onCopyBranchName} />
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

      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-6 pb-4 pt-2">
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
        <footer className="shrink-0 border-t border-slate-900/10 px-6 py-2 dark:border-white/10">
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
