import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { useArgs } from "storybook/preview-api";
import { fn } from "storybook/test";

import { SettingsPanelView, type SettingsPanelViewProps } from "./SettingsPanelView";
import { newColumnId, removeColumn } from "~/lib/board";
import { accountsNow, makeClaudeAccounts } from "./__fixtures__/accounts";
import { makeSettingsDraft, makeUpdateStatus } from "./__fixtures__/appConfig";
import { makeBoardColumns } from "./__fixtures__/board";
import { imagesNow, makeImages } from "./__fixtures__/images";
import { makeOperation } from "./__fixtures__/operations";
import { makeStoryLink } from "./__fixtures__/storyLinks";

/** Everything a story edits, rebuilt per story: the form the sections write back into, the
 *  account rows the two lists reorder, and the image list. One set behind every story is how
 *  an edit in one shows up in the next. */
function base() {
  return {
    draft: makeSettingsDraft(),
    accounts: makeClaudeAccounts(accountsNow),
    images: makeImages(),
    boardColumns: makeBoardColumns(),
    boardColumnCounts: { todo: 3, doing: 3, blocked: 1, archived: 0 },
  };
}

/** A self-update op mid-flight, so the inline progress bar under the Update button renders. */
function makeUpdateOp() {
  return makeOperation({
    id: "op_update_1",
    kind: "update",
    target: "rmng-control",
    status: "running",
    step: "pull",
    pct: 45,
    message: "pulling pegasis0/rmng:latest",
    log: ["queued self-update", "pulling pegasis0/rmng:latest"],
  });
}

/** The board-columns editor is controlled, so the story holds the list. Adding, renaming,
 *  reordering and deleting all take effect here, and still log to the Actions panel. */
function useBoardColumns(args: SettingsPanelViewProps) {
  const [columns, setColumns] = useState(args.boardColumns ?? []);
  return {
    boardColumns: columns,
    onAddBoardColumn: (title: string) => {
      setColumns((prev) => [
        ...prev,
        { id: newColumnId(title, prev), title, cloneIds: [], archive: false },
      ]);
      args.onAddBoardColumn?.(title);
    },
    onRenameBoardColumn: (columnId: string, title: string) => {
      setColumns((prev) => prev.map((c) => (c.id === columnId ? { ...c, title } : c)));
      args.onRenameBoardColumn?.(columnId, title);
    },
    onSetBoardColumnArchive: (columnId: string, archive: boolean) => {
      setColumns((prev) => prev.map((c) => (c.id === columnId ? { ...c, archive } : c)));
      args.onSetBoardColumnArchive?.(columnId, archive);
    },
    onDeleteBoardColumn: (columnId: string) => {
      setColumns((prev) => removeColumn(prev, columnId));
      args.onDeleteBoardColumn?.(columnId);
    },
    onReorderBoardColumns: (ids: string[]) => {
      setColumns((prev) =>
        ids.flatMap((id) => {
          const column = prev.find((c) => c.id === id);
          return column ? [column] : [];
        }),
      );
      args.onReorderBoardColumns?.(ids);
    },
  };
}

const meta = {
  title: "Settings/Components/SettingsPanelView",
  component: SettingsPanelView,
  parameters: { layout: "fullscreen" },
  args: {
    ...base(),
    onDraftChange: fn(),
    // Which pane a story opens on. The rail is live in every story (see `render` below), so
    // this is the starting point rather than a fixed one.
    category: "board" as const,
    onCategoryChange: fn(),
    // Nothing dragged, so the two account lists keep the order the rows arrive in.
    accountOrder: {},
    onReorderAccounts: fn(),
    onDeleteAccount: fn(),
    onDeleteCodexAccount: fn(),
    // Importing an account opens a modal ON TOP of this panel, which is navigation: the
    // story jumps to that modal's own story rather than stacking it here.
    onImportAccount: makeStoryLink("Settings/Components/ImportAccountModalView", "SignedIn"),
    setupComplete: true,
    error: null,
    restartRequired: false,
    saving: false,
    saved: false,
    onSave: fn(),
    onClose: fn(),
    serverStatus: makeUpdateStatus(),
    serverMessage: null,
    updateOperation: null,
    updateDisabled: true,
    onCheckUpdate: fn(),
    onUpdateServer: fn(),
    onRestartServer: fn(),
    testMessage: null,
    onTestDocker: fn(),
    imagesLoading: false,
    pullBusy: false,
    now: imagesNow,
    onPullLatestImage: fn(),
    onPullOtherImage: fn(),
    onDeleteImage: fn(),
    onAddBoardColumn: fn(),
    onRenameBoardColumn: fn(),
    onSetBoardColumnArchive: fn(),
    onDeleteBoardColumn: fn(),
    onReorderBoardColumns: fn(),
  },
  render: function Render(args) {
    const columns = useBoardColumns(args);
    // The rail switches panes in every story, and the Controls panel follows along, so a
    // story is a starting pane rather than the only one it can show.
    const [{ category }, updateArgs] = useArgs<SettingsPanelViewProps>();
    return (
      <SettingsPanelView
        {...args}
        {...columns}
        category={category}
        onCategoryChange={(next) => {
          updateArgs({ category: next });
          args.onCategoryChange(next);
        }}
      />
    );
  },
} satisfies Meta<typeof SettingsPanelView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The panel as it opens: the rail on the left, the Board pane beside it, and the footer
 *  pinned under both. Every other pane is one click away. */
export const Default: Story = { args: { ...base() } };

/** Agents: the two prompt layers, which are the panel's longest pane. The rail stays put
 *  while they scroll. */
export const Agents: Story = { args: { ...base(), category: "agents" } };

/** Presets: one section, and the one that grows without limit. A workspace per row. */
export const Presets: Story = { args: { ...base(), category: "presets" } };

/** Claude: the provider's polling, its imported accounts, and the pools built from them,
 *  which used to be three sections scattered down one scroll. */
export const Claude: Story = { args: { ...base(), category: "claude" } };

/** Codex: the same three sections for the other provider. */
export const Codex: Story = { args: { ...base(), category: "codex" } };

/** Clones: what a new clone is cut from, and the images it is cut out of. */
export const Clones: Story = { args: { ...base(), category: "clones" } };

/** Server: the control-server's own version and the settings it reads at startup. */
export const Server: Story = { args: { ...base(), category: "server" } };

/** A page with no board (the setup wizard's reuse of this panel). The rail drops the Board
 *  category, and asking for it anyway lands on the first one that is left. */
export const NoBoard: Story = {
  args: { ...base(), boardColumns: undefined, category: "board" },
};

/** The config is still in flight. One line where the rail and the pane will be, and no
 *  footer: there is nothing to save yet. */
export const Loading: Story = {
  args: { ...base(), draft: null },
};

/** After a save that touched a restart-required setting. The banner sits above the rail, so
 *  it stands whichever pane the save was made from. */
export const RestartRequired: Story = {
  args: { ...base(), restartRequired: true, category: "server" },
};

/** First-run setup: subnet is still editable (not yet baked in). */
export const PreSetup: Story = {
  args: { ...base(), setupComplete: false, category: "clones" },
};

/** Nothing imported and no groups configured. The account list and the group editor both
 *  show their empty states, and the group editor prompts to import accounts first. Codex
 *  is the same story one rail item down. */
export const NoAccounts: Story = {
  args: {
    ...base(),
    accounts: [],
    draft: makeSettingsDraft({ claudeGroups: [], codexGroups: [] }),
    category: "claude",
  },
};

/** A self-update in flight: its progress renders inline under the Update button, so the
 *  operator doesn't have to watch the sidebar to know how far along the restart is. */
export const UpdateInProgress: Story = {
  args: {
    ...base(),
    serverStatus: makeUpdateStatus({ available: true }),
    serverMessage: "updating… the server will restart shortly",
    updateOperation: makeUpdateOp(),
    updateDisabled: true,
    category: "server",
  },
};

/** The load or the save failed. The banner sits above the rail and the form stays exactly as
 *  it was, so the attempt can be retried as it stands. Opened on Clones, where the failed
 *  Docker probe reports too. */
export const WithError: Story = {
  args: {
    ...base(),
    error: "PUT /api/config: 400 subnet is fixed after first-run setup",
    testMessage: "✗ docker: permission denied on /var/run/docker.sock",
    category: "clones",
  },
};

/** Mid-save, then just after. Save holds its label and takes no second click; the tick in
 *  the footer is what the container clears a couple of seconds later. */
export const Saving: Story = {
  args: { ...base(), saving: true },
};

/** The panel wired to local state instead of the container: every field really edits, adding
 *  a preset or a pool really appends one, and Save locks the footer for a beat the way the
 *  real PUT does. */
export const Interactive: Story = {
  args: { ...base() },
  render: function Render(args) {
    const columns = useBoardColumns(args);
    const [draft, setDraft] = useState(args.draft);
    const [saving, setSaving] = useState(false);
    const [saved, setSaved] = useState(false);
    const [accountOrder, setAccountOrder] = useState(args.accountOrder);
    const [category, setCategory] = useState(args.category);
    return (
      <SettingsPanelView
        {...args}
        {...columns}
        category={category}
        onCategoryChange={(next) => {
          setCategory(next);
          args.onCategoryChange(next);
        }}
        draft={draft}
        onDraftChange={(key, value) => {
          setDraft((d) => (d ? { ...d, [key]: value } : d));
          args.onDraftChange(key, value);
        }}
        accountOrder={accountOrder}
        onReorderAccounts={(provider, ids) => {
          setAccountOrder((prev) => ({ ...prev, [provider]: ids }));
          args.onReorderAccounts(provider, ids);
        }}
        saving={saving}
        saved={saved}
        onSave={() => {
          setSaving(true);
          setSaved(false);
          args.onSave();
          // The point is the shape of a save, not the server: lock the footer, then let go
          // and flash the tick.
          window.setTimeout(() => {
            setSaving(false);
            setSaved(true);
            window.setTimeout(() => setSaved(false), 2500);
          }, 900);
        }}
      />
    );
  },
};
