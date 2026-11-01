import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { chatLocale, chatMessages, chatNow } from "~/components/__fixtures__/chat";
import { cloneIdle, cloneWorking } from "~/components/__fixtures__/clones";
import { makeNotesBlocks } from "~/components/__fixtures__/notes";
import { teamLabels, teamStates, ticketDetailed, ticketSubIssue } from "~/components/__fixtures__/tickets";
import { ChatView } from "~/components/ChatView";
import { NotesEditorView } from "~/components/NotesEditorView";
import TicketDescription from "~/components/TicketDescription";
import { TicketPanel } from "~/components/TicketPanel";
import type { LinearTicket } from "~/lib/tickets";
import { SIDE_DEFAULT } from "~/lib/sidePanelWidth";

import { FakeDesktop } from "../__fixtures__/desktop";
import { DESKTOP } from "../__fixtures__/screen";
import { NotesView } from "./NotesView";
import { CONTROL_PANEL_BACKDROP } from "./ui";
import { SidePanels, sideWidthPx } from "./WideLayout";

/** A ticket's panel, as the control panel's container draws it: the real description editor. */
function Ticket({ ticket, own }: { ticket: LinearTicket; own: boolean }) {
  return (
    <TicketPanel
      ticket={ticket}
      onCopyBranchName={async () => true}
      onCopyTicketLink={async () => true}
      description={<TicketDescription markdown={ticket.description ?? ""} onSave={fn()} />}
      onTitleChange={fn()}
      onStateChange={fn()}
      stateOptions={teamStates}
      labelOptions={teamLabels}
      onAddLabel={fn()}
      onRemoveLabel={fn()}
      // The clone's own ticket has its clone already
      onCreateClone={own ? undefined : fn()}
    />
  );
}

/** The wide /ng side panels (`SidePanels`), at the right of a desktop's display, as the mouse
 *  at its right edge brings them over the clone's screen (`onControlPanel`: on the control panel's backdrop).
 *  A click or a focus in a card gives it three quarters of the height; the band on their left
 *  edge sets their width. The look is the toolbar's Theme. */
function Side({
  content,
  archived,
  onControlPanel,
}: {
  /** The clone's notes (its editor), its own ticket in their place, an open ticket, or empty notes. */
  content: "notes" | "cloneTicket" | "openTicket" | "emptyNotes";
  /** The notes are an archived clone's, opened from Clones. */
  archived: boolean;
  onControlPanel: boolean;
}) {
  const [blocks] = useState(makeNotesBlocks);
  const [width, setWidth] = useState(sideWidthPx(DESKTOP.width, SIDE_DEFAULT));
  const clone = archived ? { ...cloneIdle, archived: true } : cloneWorking;
  const notes = (
    <NotesEditorView
      key={content}
      initialContent={content === "emptyNotes" ? undefined : blocks}
      onChange={fn()}
      uploadFile={async (f) => URL.createObjectURL(f)}
    />
  );
  const view = (only: "notes" | "chat") => (
    <NotesView
      only={only}
      clone={clone}
      pane={only}
      notesLabel={content === "cloneTicket" ? "Ticket" : "Notes"}
      notes={notes}
      ticketCard={content === "cloneTicket" ? <Ticket ticket={ticketSubIssue} own /> : undefined}
      chat={
        <ChatView
          messages={chatMessages}
          input=""
          onInputChange={fn()}
          scheduleAt=""
          onScheduleAtChange={fn()}
          onSend={fn()}
          onSchedule={fn()}
          onStop={fn()}
          onCancelScheduled={fn()}
          now={chatNow}
          locale={chatLocale}
          glass
        />
      }
      onBackToScreen={archived ? fn() : undefined}
    />
  );
  return (
    <div className="ng-theme relative overflow-hidden bg-black" style={{ width: DESKTOP.width, height: DESKTOP.height }}>
      <FakeDesktop />
      {onControlPanel ? <div className={`absolute inset-0 ${CONTROL_PANEL_BACKDROP}`} /> : null}
      {/* Where the screen puts them: under the buttons' row, 8 px in from the right */}
      <div className="absolute bottom-2 right-2 top-[60px]">
        <SidePanels
          ticket={content === "openTicket" ? <Ticket ticket={ticketDetailed} own={false} /> : undefined}
          notes={view("notes")}
          chat={view("chat")}
          width={width}
          onWidth={(px) => setWidth(Math.max(320, Math.min(px, DESKTOP.width * 0.6)))}
          onWidthDone={fn()}
          onWidthReset={() => setWidth(sideWidthPx(DESKTOP.width, SIDE_DEFAULT))}
        />
      </div>
    </div>
  );
}

const meta = {
  title: "ng/Control panel/SidePanels",
  component: Side,
  parameters: { layout: "centered" },
  args: { content: "notes", archived: false, onControlPanel: false },
  argTypes: { content: { control: "inline-radio", options: ["notes", "cloneTicket", "openTicket", "emptyNotes"] } },
} satisfies Meta<typeof Side>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The clone's notes (the real editor) over its assistant (the real chat). */
export const NotesAndAssistant: Story = {};

/** A clone made from a ticket: its ticket fills the notes card, with no name over it. */
export const CloneTicket: Story = { args: { content: "cloneTicket" } };

/** A ticket opened from the Tickets column takes the whole side. */
export const OpenTicket: Story = { args: { content: "openTicket", onControlPanel: true } };

/** An archived clone's notes, opened from Clones: the way back to the clone on the screen. */
export const ArchivedClone: Story = { args: { archived: true } };

/** A clone whose notes nobody wrote yet. */
export const EmptyNotes: Story = { args: { content: "emptyNotes" } };

/** With the control panel opens: on their backdrop. */
export const OnControlPanel: Story = { args: { onControlPanel: true } };
