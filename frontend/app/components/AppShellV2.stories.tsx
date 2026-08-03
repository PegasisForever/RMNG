import type { Meta, StoryObj } from "@storybook/react-vite";
import { cloneElement, isValidElement, useState, type ReactNode } from "react";
import { fn } from "storybook/test";

import { AppShellV2, type SideFocus } from "./AppShellV2";
import { ChatView } from "./ChatView";
import { NotesEditorView } from "./NotesEditorView";
import { TicketPanel } from "./TicketPanel";
import TicketDescription from "./TicketDescription";
import { moveCard } from "~/lib/board";
import { SIDE_DEFAULT } from "~/lib/sidePanelWidth";
import { openTickets, orderTickets, type LinearTicket } from "~/lib/tickets";
import type { Clone } from "~/lib/types";
import {
  accountsNow,
  makeClaudeAccounts,
  makeCloneGroups,
  makeCodexGroups,
} from "./__fixtures__/accounts";
import { makeBoardColumn, makeBoardColumns } from "./__fixtures__/board";
import { chatActivity, chatMessages, chatNow, scheduledMessages } from "./__fixtures__/chat";
import { cloneWorking, hosts, makeCloneNoToken } from "./__fixtures__/clones";
import { makeNotesBlocks } from "./__fixtures__/notes";
import { cloneOperation } from "./__fixtures__/operations";
import { cloneTokens, lxcStats, stats } from "./__fixtures__/stats";
import { makeStoryLink } from "./__fixtures__/storyLinks";
import { linearTickets, ticketCloned, ticketDetailed } from "./__fixtures__/tickets";

/** A clone that starts out in the archived column, so the fixed right column has something
 *  in it to drag back out. */
function makeArchivedClone(): Clone {
  return makeCloneNoToken({
    id: "pega-spike-4",
    displayName: "Spike: swap the encoder to VA-API",
    archived: true,
  });
}

/** The fleet the board draws, rebuilt per story. Archiving is a flag on the clone, so one
 *  list behind every story is how a card dragged into Archived in one story turns up already
 *  archived in the next. */
function makeBoardClones(): Clone[] {
  return [...hosts, makeArchivedClone()];
}

/** The chat pane on fixtures instead of the per-clone SSE stream.
 *
 *  `locale` is stamped in by the render below rather than pinned here, so the toolbar moves
 *  the timestamps in this pane as well as the reset tooltips in the rail. Without it the
 *  toolbar changed half the page. */
function ChatFixture({
  busy = false,
  archived = false,
  locale = "en-GB",
}: {
  busy?: boolean;
  archived?: boolean;
  locale?: string;
}) {
  const [input, setInput] = useState("");
  const [scheduleAt, setScheduleAt] = useState("");
  const [scheduled, setScheduled] = useState(scheduledMessages);
  return (
    <ChatView
      messages={chatMessages}
      busy={busy}
      activity={busy ? chatActivity : null}
      archived={archived}
      scheduled={scheduled}
      input={input}
      onInputChange={setInput}
      scheduleAt={scheduleAt}
      onScheduleAtChange={setScheduleAt}
      onSend={fn()}
      onSchedule={fn()}
      onStop={fn()}
      onCancelScheduled={(id) => setScheduled((s) => s.filter((m) => m.id !== id))}
      now={chatNow}
      locale={locale}
    />
  );
}

/** Re-stamp the locale onto a pane that arrives as a ready-made element. The shell takes a
 *  `locale` of its own now, and the preview's toolbar overrides it like any other arg. The
 *  pane is an element rather than a prop, so somebody has to clone it to change what is
 *  inside. The preview's decorator could: it has `ctx.args` and could hand back a cloned
 *  `chat`. It has no business knowing this one component has an arg by that name, so the
 *  clone lives here, next to the stories that build the pane. */
function withLocale(node: ReactNode, locale: string): ReactNode {
  return isValidElement<{ locale?: string }>(node) ? cloneElement(node, { locale }) : node;
}

/** The notes pane on a sample document. Edits go nowhere — no autosave, no upload. */
function NotesFixture() {
  return (
    <NotesEditorView
      initialContent={makeNotesBlocks()}
      onChange={fn()}
      uploadFile={async () => "data:image/gif;base64,R0lGODlhAQABAAAAACw="}
    />
  );
}

// Opening an overlay is navigation, so each of these jumps to that overlay's own story rather
// than rendering it on top of the board. The page story then shows the page, and every state
// a dialog can be in lives in one place instead of being reachable only through here.
const toCloneModal = makeStoryLink("Clone/Components/CloneModalView", "Default");
const toCloneModalFromTicket = makeStoryLink("Clone/Components/CloneModalView", "FromTicket");
const toSettings = makeStoryLink("Settings/Components/SettingsPanelView", "Default");
const toImportAccount = makeStoryLink("Settings/Components/ImportAccountModalView", "SignedIn");
const toChangeAccount = makeStoryLink("Clone/Components/ChangeAccountModalView", "BothProviders");
const toPortForward = makeStoryLink("Modals/Components/PortForwardModal", "Default");
const toTicketModal = makeStoryLink("Board/Components/TicketModalView", "Default");
// A ticket card fills the side panel with the ticket, which is a state of this shell rather
// than a dialog somewhere else — so it links to this shell's own story for it.
const toTicketOpen = makeStoryLink("Dashboard/Pages/AppShellV2", "TicketOpen");

/** A ticket panel on fixtures. `onCreateClone` is the caller's: the column's panel offers the
 *  button, and a clone's own notes card does not, because that clone is the one it would make. */
function TicketFixture({
  ticket,
  onCreateClone,
}: {
  ticket: LinearTicket;
  onCreateClone?: () => void;
}) {
  return (
    <TicketPanel
      ticket={ticket}
      description={
        <TicketDescription key={ticket.id} markdown={ticket.description ?? ""} onSave={fn()} />
      }
      onCopyBranchName={fn(async () => true)}
      onCreateClone={onCreateClone}
      onTitleChange={fn()}
    />
  );
}

/** The control rail's props, built fresh per story. The account lists and the pools reach
 *  components that hold them in state, so one array behind every story is how an edit in one
 *  story leaks into the next. */
function makeRail() {
  return {
    accounts: makeClaudeAccounts(accountsNow),
    // The container subscribes to the shared order store and passes the value; nothing has
    // been dragged in a story, so the accounts stay in the order they arrive.
    accountOrder: {},
    cloneGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    // The container's clock. Pinned to the instant the account fixtures are written for, so
    // each bar's pace marker and reset countdown are the same on every machine and every day.
    now: accountsNow,
    lxcStats,
    operations: [],
    presetNames: ["Default", "Focus"],
    activeLayout: "Default",
    onActivateLayout: fn(),
    onOpenSettings: toSettings,
    onImportAccount: toImportAccount,
    onRefresh: fn(),
  };
}

/** The ticket column's own props, built fresh per story next to the fleet the list is
 *  filtered against. The interactive story drops cards from it and stamps its own selection
 *  in, so one object behind every story is how a cancelled ticket stays gone in the next. */
function makeTicketColumn(clones: Clone[]) {
  return {
    tickets: openTickets(linearTickets, clones),
    loading: false,
    error: null,
    selectedId: null as string | null,
    onSelectTicket: toTicketOpen,
    onCancel: fn(),
    onMoveToBacklog: fn(),
    onNewTicket: toTicketModal,
    // Leaving for Linear has no destination story, so it logs the URL instead. The two copies
    // log the string rather than writing it to the real clipboard.
    onOpenInLinear: fn(),
    onCopyBranchName: fn(),
    onCopyTicketLink: fn(),
  };
}

/** The board's own props, built fresh per story. The columns are the reason: the shell copies
 *  them into state and a drag rewrites the list, so one array behind every story is how a card
 *  moved in one story shows up somewhere else in the next. */
function makeBoard() {
  const clones = makeBoardClones();
  return {
    columns: makeBoardColumns(),
    clones,
    stats,
    cloneTokens,
    operations: [],
    selectedId: cloneWorking.id as string | null,
    sshPublicHost: "rmng.example.com",
    bastionPort: 2222,
    onSelectClone: fn(),
    onDeleteClone: fn(),
    onCommitClone: fn(),
    onChangeAccountClone: toChangeAccount,
    onPortForwardClone: toPortForward,
    onArchiveClone: fn(),
    onUnarchiveClone: fn(),
    onCopySshCommand: fn(async () => true),
    onOpenInLinear: fn(),
    onNewClone: toCloneModal,
    onMoveCard: fn(),
    onRenameColumn: fn(),
    tickets: makeTicketColumn(clones),
    onNewCloneFromTicket: toCloneModalFromTicket,
    onReorderTickets: fn(),
  };
}

const meta = {
  title: "Dashboard/Pages/AppShellV2",
  component: AppShellV2,
  parameters: { layout: "fullscreen" },
  args: {
    selectedClone: cloneWorking,
    error: null,
    // The shell's own locale, which the preview's toolbar overrides on every story here.
    locale: "en-GB",
    sideFocus: "notes" as SideFocus,
    onSideFocusChange: fn(),
    // The container reads the remembered width and the shell opens at whatever it is handed,
    // so the Controls panel moves the panel edge the same way a drag does.
    initialSideWidth: SIDE_DEFAULT,
    onSideWidthCommit: fn(),
    notes: <NotesFixture />,
    chat: <ChatFixture />,
  },
  /** `rail` and `board` are deliberately absent above, so TypeScript refuses a story that does
   *  not build its own pair. A default on the meta would be one set of objects built once at
   *  module scope, and both of them reach components that hold what they are given in state.
   *
   *  The chat pane is the one thing the locale toolbar cannot reach on its own: it arrives as
   *  a ready-made element, so the render stamps the value into it. */
  render: (args) => <AppShellV2 {...args} chat={withLocale(args.chat, args.locale)} />,
} satisfies Meta<typeof AppShellV2>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The board: control rail, three operator columns, the fixed Archived column, and the
 *  selected clone's notes and chat down the right quarter. The gear, both New clone buttons
 *  and the card menus jump to their own stories. Dragging a card lives in Interactive. */
export const Default: Story = { args: { rail: makeRail(), board: makeBoard() } };

/** The agent is mid-turn, with the chat focused: it holds three quarters of the side panel
 *  and the notes shrink to a quarter. Interactive is where clicking into the notes swaps them. */
export const AgentWorking: Story = {
  args: {
    rail: makeRail(),
    board: makeBoard(),
    chat: <ChatFixture busy />,
    sideFocus: "chat",
  },
};

/** A ticket has the side panel, as one card instead of two. The clone stays selected
 *  underneath and keeps its stream, so the ticket card takes the board's highlight and the
 *  clone's card gives it up. */
export const TicketOpen: Story = {
  args: {
    rail: makeRail(),
    board: {
      ...makeBoard(),
      selectedId: null,
      tickets: { ...makeTicketColumn(makeBoardClones()), selectedId: ticketDetailed.id },
    },
    ticket: <TicketFixture ticket={ticketDetailed} onCreateClone={toCloneModalFromTicket} />,
  },
};

/** The selected clone was made from a Linear ticket, so the ticket takes the top card. It is
 *  the same card TicketOpen draws, over the same chat quarter the notes had: no clone id above
 *  it, since the ticket's own header is the card's title.
 *
 *  There is no "Create a clone" button, and the notes for this clone are not reachable from
 *  anywhere. Both are deliberate. */
export const TicketInNotesCard: Story = {
  args: {
    rail: makeRail(),
    board: makeBoard(),
    cloneTicket: <TicketFixture ticket={ticketCloned} />,
  },
};

/** No clone selected — the side panel holds its empty state. */
export const NoCloneSelected: Story = {
  args: { rail: makeRail(), board: { ...makeBoard(), selectedId: null }, selectedClone: null },
};

/** A failed action banners above the page while a clone is being provisioned, which the
 *  rail reports under Activity. */
export const WithError: Story = {
  args: {
    error: "swap account: clone pega-we-142 is not running",
    rail: { ...makeRail(), operations: [cloneOperation] },
    board: { ...makeBoard(), operations: [cloneOperation] },
  },
};

/** A fresh install: one empty column, no clones, nothing archived. */
export const EmptyBoard: Story = {
  args: {
    board: {
      ...makeBoard(),
      columns: [makeBoardColumn()],
      clones: [],
      // Rebuilt against this story's own fleet, which is empty. The column is the tickets
      // nobody has cloned yet, so a board with no clones has nothing filtered out of it.
      tickets: makeTicketColumn([]),
      selectedId: null,
    },
    rail: { ...makeRail(), accounts: [], cloneGroups: [], codexGroups: [], lxcStats: null },
    selectedClone: null,
  },
};

/** The shell wired to local state instead of the container, so the board really works: cards
 *  drag between columns, a drop into Archived sets the flag the way the server's answer would,
 *  a ticket card takes the side panel, and clicking into a card swaps the two heights. Opening
 *  a dialog still leaves for that dialog's own story. */
export const Interactive: Story = {
  args: { rail: makeRail(), board: makeBoard() },
  render: function Render(args) {
    const [columns, setColumns] = useState(args.board.columns);
    const [clones, setClones] = useState(args.board.clones);
    const [selectedId, setSelectedId] = useState(args.board.selectedId);
    const [sideFocus, setSideFocus] = useState<SideFocus>(args.sideFocus);
    // The operator's own arrangement of the ticket column. Empty means nobody has moved
    // anything yet, so the list is still Linear's order.
    const [ticketOrder, setTicketOrder] = useState<string[]>([]);
    // Tickets the story has cancelled or sent to the backlog. The real container drops them
    // the same way: the write lands in Linear, and the card goes without waiting for a
    // fetch to confirm what was just asked for.
    const [dropped, setDropped] = useState<string[]>([]);
    // The ticket whose panel has the side column. Null = the clone's notes and chat have it.
    const [openTicket, setOpenTicket] = useState<LinearTicket | null>(null);

    const setArchived = (cloneId: string, archived: boolean) =>
      setClones((prev) => prev.map((c) => (c.id === cloneId ? { ...c, archived } : c)));

    const selectedClone = clones.find((c) => c.id === selectedId) ?? null;

    return (
      <AppShellV2
        {...args}
        chat={withLocale(args.chat, args.locale)}
        selectedClone={selectedClone}
        ticket={
          openTicket ? (
            <TicketFixture ticket={openTicket} onCreateClone={toCloneModalFromTicket} />
          ) : undefined
        }
        sideFocus={sideFocus}
        onSideFocusChange={(next) => {
          setSideFocus(next);
          args.onSideFocusChange(next);
        }}
        board={{
          ...args.board,
          columns,
          clones,
          tickets: {
            ...args.board.tickets,
            tickets: orderTickets(openTickets(linearTickets, clones), ticketOrder).filter(
              (t) => !dropped.includes(t.id),
            ),
            selectedId: openTicket?.id ?? null,
            onSelectTicket: setOpenTicket,
            onCancel: (ticket) => {
              setDropped((prev) => [...prev, ticket.id]);
              args.board.tickets?.onCancel?.(ticket);
            },
            onMoveToBacklog: (ticket) => {
              setDropped((prev) => [...prev, ticket.id]);
              args.board.tickets?.onMoveToBacklog?.(ticket);
            },
          },
          onReorderTickets: (order) => {
            setTicketOrder(order);
            args.board.onReorderTickets?.(order);
          },
          // A ticket owns the highlight while it is open, and the clone keeps its stream
          // underneath. Picking any clone, the selected one included, closes the ticket.
          selectedId: openTicket ? null : selectedId,
          onSelectClone: (clone) => {
            setOpenTicket(null);
            setSelectedId(clone.id);
            args.board.onSelectClone(clone);
          },
          onMoveCard: (cloneId, toColumnId, toIndex) => {
            setColumns((prev) => moveCard(prev, cloneId, toColumnId, toIndex));
            args.board.onMoveCard(cloneId, toColumnId, toIndex);
          },
          onArchiveClone: (clone) => {
            setArchived(clone.id, true);
            args.board.onArchiveClone(clone);
          },
          onUnarchiveClone: (clone) => {
            setArchived(clone.id, false);
            args.board.onUnarchiveClone(clone);
          },
          onRenameColumn: (columnId, title) => {
            setColumns((prev) => prev.map((c) => (c.id === columnId ? { ...c, title } : c)));
            args.board.onRenameColumn(columnId, title);
          },
        }}
      />
    );
  },
};
