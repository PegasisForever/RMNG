// The /ng Notes page: the clone's notes (or the ticket it was made from) and its assistant
// chat. The desktop stacks the two in its side panel; this page is too short for two, so a
// switch in the header shows one at a time, and the other stays as it was.
//
// Wide (`stacked`), it is the dashboard's side panel: the notes over the chat, both shown.
//
// It is about the clone on the screen above. An archived clone has no screen, so a tap on one
// in the Clones page opens its notes here, and the header then says so and offers the way
// back to the clone on the screen.
//
// Pure: the notes, the ticket and the chat come in as slots (the real ones load lazily, in the
// browser only).
import type { ReactNode } from "react";

import { CloneStatusDot, statusLabel } from "~/components/mobile/CloneStatus";
import type { CloneTicket } from "~/lib/tickets";
import type { Clone } from "~/lib/types";

import { EmptyLine, HeaderButton, LINE, Page, PageHeader, Segmented, TEXT_3 } from "./ui";

/** Which of the two shows. */
export type NotesPane = "notes" | "chat";

export interface NotesViewProps {
  /** The clone the page is about; null when no clone is selected. */
  clone: Clone | null;
  ticket?: CloneTicket;
  pane: NotesPane;
  onPaneChange: (pane: NotesPane) => void;
  /** "Ticket" for a clone made from a ticket: its ticket is where its notes would be. */
  notesLabel: string;
  notes: ReactNode;
  chat: ReactNode;
  /** The page is on an archived clone, not the one on the screen: the way back to that one. */
  onBackToScreen?: () => void;
  /** The notes over the chat, both shown (wide), not one at a time. */
  stacked?: boolean;
}

export function NotesView({
  clone,
  ticket,
  pane,
  onPaneChange,
  notesLabel,
  notes,
  chat,
  onBackToScreen,
  stacked = false,
}: NotesViewProps) {
  if (!clone) {
    return (
      <Page header={<PageHeader title="Notes and assistant" />}>
        <EmptyLine>No clone is selected. Pick one in Clones.</EmptyLine>
      </Page>
    );
  }
  return (
    <Page
      fill
      header={
        <PageHeader
          title={
            <span className="flex items-center gap-1.5">
              <CloneStatusDot clone={clone} />
              <span className="truncate">{ticket?.title ?? clone.displayName ?? clone.id}</span>
            </span>
          }
          sub={`${clone.id} · ${statusLabel(clone)}`}
        >
          {onBackToScreen ? <HeaderButton label="Back to screen" onClick={onBackToScreen} /> : null}
          {stacked ? null : <Segmented
            options={[
              { value: "notes", label: notesLabel },
              { value: "chat", label: "Assistant" },
            ]}
            value={pane}
            onChange={onPaneChange}
          />}
        </PageHeader>
      }
    >
      {/* The notes are one long scroll; the chat is a column with its composer at the bottom */}
      {stacked ? (
        <>
          <div className="min-h-0 flex-[3] overflow-y-auto overscroll-y-contain py-2">{notes}</div>
          <div className={`flex min-h-0 flex-[2] flex-col border-t ${LINE}`}>
            <h3 className={`shrink-0 px-3 pt-2 text-[11px] font-semibold uppercase tracking-wide ${TEXT_3}`}>Assistant</h3>
            {chat}
          </div>
        </>
      ) : pane === "notes" ? (
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain py-2">{notes}</div>
      ) : (
        chat
      )}
    </Page>
  );
}
