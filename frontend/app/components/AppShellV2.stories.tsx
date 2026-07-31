import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { AppShellV2, type ShellPane, type SideFocus } from "./AppShellV2";
import { ChatView } from "./ChatView";
import { NotesEditor } from "./NotesEditor";
import { SettingsPanel } from "./SettingsPanel";
import { moveCard, newColumnId, removeColumn, resolveColumns } from "~/lib/board";
import type { Clone } from "~/lib/types";
import {
  boardColumns,
  chatMessages,
  chatNow,
  claudeAccounts,
  cloneGroups,
  cloneNoToken,
  appConfig,
  cloneOperation,
  cloneTokens,
  cloneWorking,
  codexGroups,
  hosts,
  images,
  lxcStats,
  notesBlocks,
  scheduledMessages,
  stats,
} from "~/stories/fixtures";

/** A clone that starts out in the archived column, so the fixed right column has something
 *  in it to drag back out. */
const archivedClone: Clone = {
  ...cloneNoToken,
  id: "pega-spike-4",
  displayName: "Spike: swap the encoder to VA-API",
  archived: true,
};

const boardClones: Clone[] = [...hosts, archivedClone];

/** The chat pane on fixtures instead of the per-clone SSE stream. */
function ChatFixture({ busy = false, archived = false }: { busy?: boolean; archived?: boolean }) {
  const [input, setInput] = useState("");
  const [scheduleAt, setScheduleAt] = useState("");
  const [scheduled, setScheduled] = useState(scheduledMessages);
  return (
    <ChatView
      messages={chatMessages}
      busy={busy}
      activity={busy ? "Bash(bun run build)" : null}
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
    />
  );
}

/** The notes pane on a sample document. Edits go nowhere — no autosave, no upload. */
function NotesFixture() {
  return (
    <NotesEditor
      initialContent={notesBlocks}
      onChange={fn()}
      uploadFile={async () => "data:image/gif;base64,R0lGODlhAQABAAAAACw="}
    />
  );
}

/** The settings modal's injected server calls. The board-columns section is the reason the
 *  gear is wired up here at all; the rest are stubs so the panel renders. */
const settingsStubs = {
  getConfig: async () => appConfig,
  putConfig: async () => ({ config: appConfig, restartRequired: false }),
  testConfig: async () => ({ ok: true, message: "Docker reachable" }),
  getUpdateStatus: async () => ({
    currentRevision: "a1b2c3d",
    currentCreated: "2026-07-01T12:00:00Z",
    currentDigest: "sha256:1111",
    remoteDigest: "sha256:1111",
    available: false,
    reference: "pegasis0/rmng:latest",
    error: null,
  }),
  updateServer: async () => cloneOperation,
  restartServer: async () => ({ ok: true }),
  images,
  imagesLoading: false,
  pullBusy: false,
  onPullTemplate: fn(),
  onDeleteImage: fn(),
  onDeleteAccount: fn(),
  onDeleteCodexAccount: fn(),
  onImportAccount: fn(),
};

const rail = {
  accounts: claudeAccounts,
  cloneGroups,
  codexGroups,
  lxcStats,
  operations: [],
  presetNames: ["Default", "Focus"],
  activeLayout: "Default",
  onActivateLayout: fn(),
  onOpenSettings: fn(),
  onImportAccount: fn(),
  onRefresh: fn(),
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
  onChangeAccountClone: fn(),
  onPortForwardClone: fn(),
  onArchiveClone: fn(),
  onUnarchiveClone: fn(),
  onNewClone: fn(),
  onMoveCard: fn(),
  onRenameColumn: fn(),
};

const meta = {
  title: "Page/AppShellV2",
  component: AppShellV2,
  parameters: { layout: "fullscreen" },
  args: {
    board,
    rail,
    selectedClone: cloneWorking,
    error: null,
    pane: "board" as ShellPane,
    onPaneChange: fn(),
    sideFocus: "notes" as SideFocus,
    onSideFocusChange: fn(),
    notes: <NotesFixture />,
    chat: <ChatFixture />,
  },
  /** The shell is controlled, so the story holds the board state: column membership,
   *  which clone is selected, and each clone's archived flag. That last one is what makes
   *  a drag into the Archived column actually stick, the way the server's answer would. */
  render: (args) => {
    const [columns, setColumns] = useState(args.board.columns);
    const [clones, setClones] = useState(args.board.clones);
    const [selectedId, setSelectedId] = useState(args.board.selectedId);
    const [pane, setPane] = useState<ShellPane>(args.pane);
    const [sideFocus, setSideFocus] = useState<SideFocus>(args.sideFocus);
    const [settingsOpen, setSettingsOpen] = useState(false);

    const setArchived = (cloneId: string, archived: boolean) =>
      setClones((prev) => prev.map((c) => (c.id === cloneId ? { ...c, archived } : c)));

    const selectedClone = clones.find((c) => c.id === selectedId) ?? null;
    // What each column actually holds right now, so the settings list can say what a
    // delete displaces (the unfiled clones count against the first column).
    const counts = Object.fromEntries(
      resolveColumns(columns, clones).map((column) => [column.id, column.cloneIds.length]),
    );

    return (
      <AppShellV2
        {...args}
        selectedClone={selectedClone}
        pane={pane}
        onPaneChange={setPane}
        sideFocus={sideFocus}
        onSideFocusChange={(next) => {
          setSideFocus(next);
          args.onSideFocusChange(next);
        }}
        rail={{
          ...args.rail,
          onOpenSettings: () => {
            setSettingsOpen(true);
            args.rail.onOpenSettings();
          },
        }}
        overlays={
          settingsOpen ? (
            <SettingsPanel
              {...settingsStubs}
              accounts={args.rail.accounts}
              operations={args.board.operations}
              onClose={() => setSettingsOpen(false)}
              boardColumns={columns}
              boardColumnCounts={counts}
              onAddBoardColumn={(title) =>
                setColumns((prev) => [
                  ...prev,
                  { id: newColumnId(title, prev), title, cloneIds: [] },
                ])
              }
              onRenameBoardColumn={(columnId, title) =>
                setColumns((prev) => prev.map((c) => (c.id === columnId ? { ...c, title } : c)))
              }
              onDeleteBoardColumn={(columnId) => setColumns((prev) => removeColumn(prev, columnId))}
              onReorderBoardColumns={(ids) =>
                setColumns((prev) =>
                  ids.flatMap((id) => {
                    const column = prev.find((c) => c.id === id);
                    return column ? [column] : [];
                  }),
                )
              }
            />
          ) : null
        }
        board={{
          ...args.board,
          columns,
          clones,
          selectedId,
          onSelectClone: (clone) => {
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
 *  selected clone's notes and chat down the right quarter. Cards drag between columns. */
export const Default: Story = {};

/** The agent is mid-turn, with the chat focused: it holds three quarters of the side panel
 *  and the notes shrink to a quarter. Clicking into the notes swaps the two. */
export const AgentWorking: Story = {
  args: { chat: <ChatFixture busy />, sideFocus: "chat" },
};

/** No clone selected — the side panel holds its empty state. */
export const NoCloneSelected: Story = {
  args: { board: { ...board, selectedId: null }, selectedClone: null },
};

/** A failed action banners above the page while a clone is being provisioned, which the
 *  rail reports under Activity. */
export const WithError: Story = {
  args: {
    error: "swap account: clone pega-we-142 is not running",
    rail: { ...rail, operations: [cloneOperation] },
    board: { ...board, operations: [cloneOperation] },
  },
};

/** A fresh install: one empty column, no clones, nothing archived. */
export const EmptyBoard: Story = {
  args: {
    board: {
      ...board,
      columns: [{ id: "todo", title: "Todo", cloneIds: [] }],
      clones: [],
      selectedId: null,
    },
    rail: { ...rail, accounts: [], cloneGroups: [], codexGroups: [], lxcStats: null },
    selectedClone: null,
  },
};
