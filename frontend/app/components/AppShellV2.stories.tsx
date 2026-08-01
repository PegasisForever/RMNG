import type { Meta, StoryObj } from "@storybook/react-vite";
import { cloneElement, isValidElement, useState, type ReactNode } from "react";
import { fn } from "storybook/test";

import { AppShellV2, type SideFocus } from "./AppShellV2";
import { ChatView } from "./ChatView";
import { NotesEditorView } from "./NotesEditorView";
import { TicketPanel } from "./TicketPanel";
import TicketDescription from "./TicketDescription";
import { moveCard } from "~/lib/board";
import { openTickets, orderTickets, type LinearTicket } from "~/lib/tickets";
import type { Clone } from "~/lib/types";
import {
  accountsNow,
  makeClaudeAccounts,
  makeCloneGroups,
  makeCodexGroups,
} from "./__fixtures__/accounts";
import { boardColumns, makeBoardColumn } from "./__fixtures__/board";
import { chatActivity, chatMessages, chatNow, scheduledMessages } from "./__fixtures__/chat";
import { cloneWorking, hosts, makeCloneNoToken } from "./__fixtures__/clones";
import { makeNotesBlocks } from "./__fixtures__/notes";
import { cloneOperation } from "./__fixtures__/operations";
import { cloneTokens, lxcStats, stats } from "./__fixtures__/stats";
import { makeStoryLink } from "./__fixtures__/storyLinks";
import { linearTickets } from "./__fixtures__/tickets";

/** A clone that starts out in the archived column, so the fixed right column has something
 *  in it to drag back out. */
const archivedClone: Clone = makeCloneNoToken({
  id: "pega-spike-4",
  displayName: "Spike: swap the encoder to VA-API",
  archived: true,
});

const boardClones: Clone[] = [...hosts, archivedClone];

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

/** Re-stamp the locale onto a pane that arrives as a ready-made element. `chat` is an arg, so
 *  the story that knows what is in it is the one that can apply the toolbar to it. */
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

/** The control rail's props, built fresh per story. The account lists and the pools reach
 *  components that hold them in state, so one array behind every story is how an edit in one
 *  story leaks into the next. */
function makeRail() {
  return {
    accounts: makeClaudeAccounts(accountsNow),
    // The route subscribes to the shared order store and passes the value; nothing has been
    // dragged in a story, so the accounts stay in the order they arrive.
    accountOrder: {},
    cloneGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    // The route reads the operator's own locale here; a story pins one so the usage bars'
    // reset tooltips read the same on every machine.
    locale: "en-GB",
    // The route's clock. Pinned to the instant the account fixtures are written for, so each
    // bar's pace marker and reset countdown are the same on every machine and every day.
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

/** The ticket column's own props, hoisted so the render can rebuild just its list as
 *  clones come and go without losing the callbacks. */
const ticketColumn = {
  tickets: openTickets(linearTickets, boardClones),
  loading: false,
  error: null,
  onCancel: fn(),
  onMoveToBacklog: fn(),
  onNewTicket: toTicketModal,
};

const board = {
  columns: boardColumns,
  clones: boardClones,
  stats,
  cloneTokens,
  operations: [],
  selectedId: cloneWorking.id,
  sshPublicHost: "rmng.example.com",
  bastionPort: 2222,
  onSelectClone: fn(),
  onDeleteClone: fn(),
  onCommitClone: fn(),
  onChangeAccountClone: toChangeAccount,
  onPortForwardClone: toPortForward,
  onArchiveClone: fn(),
  onUnarchiveClone: fn(),
  onNewClone: toCloneModal,
  onMoveCard: fn(),
  onRenameColumn: fn(),
  tickets: ticketColumn,
  onNewCloneFromTicket: toCloneModalFromTicket,
  onReorderTickets: fn(),
};

const meta = {
  title: "Dashboard/Pages/AppShellV2",
  component: AppShellV2,
  parameters: { layout: "fullscreen" },
  args: {
    board,
    selectedClone: cloneWorking,
    error: null,
    sideFocus: "notes" as SideFocus,
    onSideFocusChange: fn(),
    notes: <NotesFixture />,
    chat: <ChatFixture />,
  },
  /** The shell is controlled, so the story holds the board state: column membership,
   *  which clone is selected, and each clone's archived flag. That last one is what makes
   *  a drag into the Archived column actually stick, the way the server's answer would.
   *
   *  It also applies the locale toolbar by hand. The preview's decorator overrides an arg
   *  named `locale`, and this shell has none: it has two, one inside `rail` and one inside the
   *  chat pane it is handed. No decorator can find either, so the story that knows where they
   *  live puts the toolbar's value in both. */
  render: (args, ctx) => {
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
    const locale = String(ctx.globals.locale ?? args.rail.locale);

    const setArchived = (cloneId: string, archived: boolean) =>
      setClones((prev) => prev.map((c) => (c.id === cloneId ? { ...c, archived } : c)));

    const selectedClone = clones.find((c) => c.id === selectedId) ?? null;

    return (
      <AppShellV2
        {...args}
        rail={{ ...args.rail, locale }}
        chat={withLocale(args.chat, locale)}
        selectedClone={selectedClone}
        ticket={
          openTicket ? (
            <TicketPanel
              ticket={openTicket}
              description={
                <TicketDescription
                  key={openTicket.id}
                  markdown={openTicket.description ?? ""}
                  onSave={fn()}
                />
              }
              onCreateClone={toCloneModalFromTicket}
              onTitleChange={fn()}
            />
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
            ...ticketColumn,
            tickets: orderTickets(openTickets(linearTickets, clones), ticketOrder).filter(
              (t) => !dropped.includes(t.id),
            ),
            selectedId: openTicket?.id ?? null,
            onSelectTicket: setOpenTicket,
            onCancel: (ticket) => {
              setDropped((prev) => [...prev, ticket.id]);
              ticketColumn.onCancel(ticket);
            },
            onMoveToBacklog: (ticket) => {
              setDropped((prev) => [...prev, ticket.id]);
              ticketColumn.onMoveToBacklog(ticket);
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
} satisfies Meta<typeof AppShellV2>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The board: control rail, three operator columns, the fixed Archived column, and the
 *  selected clone's notes and chat down the right quarter. Cards drag between columns.
 *  The gear, both New clone buttons and the card menus jump to their own stories. */
export const Default: Story = { args: { rail: makeRail() } };

/** The agent is mid-turn, with the chat focused: it holds three quarters of the side panel
 *  and the notes shrink to a quarter. Clicking into the notes swaps the two. */
export const AgentWorking: Story = {
  args: { rail: makeRail(), chat: <ChatFixture busy />, sideFocus: "chat" },
};

/** No clone selected — the side panel holds its empty state. */
export const NoCloneSelected: Story = {
  args: { rail: makeRail(), board: { ...board, selectedId: null }, selectedClone: null },
};

/** A failed action banners above the page while a clone is being provisioned, which the
 *  rail reports under Activity. */
export const WithError: Story = {
  args: {
    error: "swap account: clone pega-we-142 is not running",
    rail: { ...makeRail(), operations: [cloneOperation] },
    board: { ...board, operations: [cloneOperation] },
  },
};

/** A fresh install: one empty column, no clones, nothing archived. */
export const EmptyBoard: Story = {
  args: {
    board: {
      ...board,
      columns: [makeBoardColumn()],
      clones: [],
      selectedId: null,
    },
    rail: { ...makeRail(), accounts: [], cloneGroups: [], codexGroups: [], lxcStats: null },
    selectedClone: null,
  },
};
