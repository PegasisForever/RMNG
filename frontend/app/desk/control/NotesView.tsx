// The Notes and Assistant tabs: the clone's notes (or the ticket it was made from), and its
// assistant chat. A narrow page is too short for two, so each is a tab of its own (`pane`).
//
// Wide (`only`), it is one of the two side panels (`./WideLayout.tsx`): the clone's name over
// its notes, or its ticket filling the panel; the assistant under a small label.
//
// It is about the clone on the screen above. An archived clone has no screen, so a tap on one
// in the Clones page opens its notes here, and the header then says so and offers the way
// back to the clone on the screen.
//
// Pure: the notes, the ticket and the chat come in as slots (the real ones load lazily, in the
// browser only).
import type { ReactNode } from "react";

import { CloneStatusDot, statusLabel } from "~/components/CloneStatus";
import { cloneTitle } from "~/lib/cloneTitle";
import type { CloneTicket } from "~/lib/tickets";
import type { Clone } from "~/lib/types";

import { EmptyLine, HeaderButton, NO_SCROLLBARS, Page, PageHeader, TEXT, TEXT_3 } from "./ui";

/** Which of the two shows. */
export type NotesPane = "notes" | "chat";

export interface NotesViewProps {
  /** The clone the page is about; null when no clone is selected. */
  clone: Clone | null;
  ticket?: CloneTicket;
  /** Narrow: which tab this is, the clone's notes or its assistant (each a tab of its own). */
  pane: NotesPane;
  /** "Ticket" for a clone made from a ticket: its ticket is where its notes would be. */
  notesLabel: string;
  notes: ReactNode;
  chat: ReactNode;
  /** The page is on an archived clone, not the one on the screen: the way back to that one. */
  onBackToScreen?: () => void;
  /** This one only, in a panel of its own (wide), with no switch. */
  only?: NotesPane;
  /** Wide: the clone's own ticket, which fills the notes card in place of its name and notes. */
  ticketCard?: ReactNode;
}

export function NotesView({
  clone,
  ticket,
  pane,
  notesLabel,
  notes,
  chat,
  onBackToScreen,
  only,
  ticketCard,
}: NotesViewProps) {
  if (!clone) {
    return (
      <Page header={<PageHeader title={(only ?? pane) === "chat" ? "Assistant" : notesLabel} />}>
        <EmptyLine>No clone is selected. Pick one in Clones.</EmptyLine>
      </Page>
    );
  }
  if (only === "chat") {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <h3 className={`shrink-0 px-4 pt-3 text-[11px] font-semibold uppercase tracking-wide ${TEXT_3}`}>Assistant</h3>
        {chat}
      </div>
    );
  }
  if (only === "notes") {
    // No name over a ticket: its own header is the card's title
    if (ticketCard && !onBackToScreen) return <div className="flex h-full min-h-0 flex-col">{ticketCard}</div>;
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex shrink-0 items-center gap-2 px-4 pt-3">
          <h2 className={`min-w-0 flex-1 truncate text-sm font-semibold ${TEXT}`}>{clone.id}</h2>
          {onBackToScreen ? <HeaderButton label="Back to screen" onClick={onBackToScreen} /> : null}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain py-2">{ticketCard ?? notes}</div>
      </div>
    );
  }
  return (
    // The notes and the chat scroll with no bars
    <div className={`h-full ${NO_SCROLLBARS}`}>
      <Page
        fill
        header={
          <PageHeader
            title={
              <span className="flex items-center gap-1.5">
                <CloneStatusDot clone={clone} />
                <span className="truncate">{cloneTitle(clone, ticket)}</span>
              </span>
            }
            sub={`${clone.id} · ${statusLabel(clone)}`}
          >
            {onBackToScreen ? <HeaderButton label="Back to screen" onClick={onBackToScreen} /> : null}
          </PageHeader>
        }
      >
        {/* The notes are one long scroll; the chat is a column with its composer at the bottom */}
        {pane === "notes" ? (
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain py-2">{notes}</div>
        ) : (
          chat
        )}
      </Page>
    </div>
  );
}
