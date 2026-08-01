import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SettingsPanel } from "./SettingsPanel";
import { newColumnId, removeColumn } from "~/lib/board";
import type { Operation } from "~/lib/types";
import { makeClaudeAccounts } from "./__fixtures__/accounts";
import { makeAppConfig } from "./__fixtures__/appConfig";
import { boardColumns } from "./__fixtures__/board";
import { images } from "./__fixtures__/images";

// Mocked server calls — the component never imports the real API, so a story just
// injects these. `fn(impl)` both runs the implementation and records the call in the
// Actions panel.
// Each call builds its own config, the way a real fetch hands back its own parse. The panel
// edits what it is given, so a stub that answered one shared object would let the Default
// story's edits show up in RestartRequired.
const getConfig = () => fn(async () => makeAppConfig());
const putConfig = (restartRequired = false) =>
  fn(async () => ({ config: makeAppConfig(), restartRequired }));
const testConfig = () =>
  fn(async () => ({ ok: true, message: "Docker reachable (Engine 27.1.1)" }));
const getUpdateStatus = () =>
  fn(async () => ({
    currentRevision: "a1b2c3d",
    currentCreated: "2026-07-01T12:00:00Z",
    currentDigest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    remoteDigest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    available: false,
    reference: "pegasis0/rmng:latest",
    error: null,
  }));

// A self-update op mid-flight, so the inline progress bar under the Update button renders.
const updateOp: Operation = {
  id: "op_update_1",
  kind: "update",
  target: "rmng-control",
  status: "running",
  step: "pull",
  pct: 45,
  message: "pulling pegasis0/rmng:latest",
  log: ["queued self-update", "pulling pegasis0/rmng:latest"],
  startedAt: 0,
};

const meta = {
  title: "Settings/Components/SettingsPanel",
  component: SettingsPanel,
  parameters: { layout: "fullscreen" },
  args: {
    // The flat both-provider list the panel splits into its Claude / Codex account sections.
    accounts: makeClaudeAccounts(),
    onClose: fn(),
    getConfig: getConfig(),
    putConfig: putConfig(),
    testConfig: testConfig(),
    getUpdateStatus: getUpdateStatus(),
    updateServer: fn(async () => updateOp),
    // No op in flight in the default story; `UpdateInProgress` supplies one.
    operations: [],
    restartServer: fn(async () => ({ ok: true })),
    images,
    imagesLoading: false,
    pullBusy: false,
    onPullTemplate: fn(),
    onDeleteImage: fn(),
    onDeleteAccount: fn(),
    onDeleteCodexAccount: fn(),
    onImportAccount: fn(),
    boardColumns,
    boardColumnCounts: { todo: 3, doing: 3, blocked: 1, archived: 0 },
    onAddBoardColumn: fn(),
    onRenameBoardColumn: fn(),
    onSetBoardColumnArchive: fn(),
    onDeleteBoardColumn: fn(),
    onReorderBoardColumns: fn(),
  },
  /** The board-columns editor is controlled, so the story holds the list. Adding, renaming,
   *  reordering and deleting all take effect here, and still log to the Actions panel. */
  render: (args) => {
    const [columns, setColumns] = useState(args.boardColumns ?? []);
    return (
      <SettingsPanel
        {...args}
        boardColumns={columns}
        onAddBoardColumn={(title) => {
          setColumns((prev) => [
            ...prev,
            { id: newColumnId(title, prev), title, cloneIds: [], archive: false },
          ]);
          args.onAddBoardColumn?.(title);
        }}
        onRenameBoardColumn={(columnId, title) => {
          setColumns((prev) => prev.map((c) => (c.id === columnId ? { ...c, title } : c)));
          args.onRenameBoardColumn?.(columnId, title);
        }}
        onSetBoardColumnArchive={(columnId, archive) => {
          setColumns((prev) => prev.map((c) => (c.id === columnId ? { ...c, archive } : c)));
          args.onSetBoardColumnArchive?.(columnId, archive);
        }}
        onDeleteBoardColumn={(columnId) => {
          setColumns((prev) => removeColumn(prev, columnId));
          args.onDeleteBoardColumn?.(columnId);
        }}
        onReorderBoardColumns={(ids) => {
          setColumns((prev) =>
            ids.flatMap((id) => {
              const column = prev.find((c) => c.id === id);
              return column ? [column] : [];
            }),
          );
          args.onReorderBoardColumns?.(ids);
        }}
      />
    );
  },
} satisfies Meta<typeof SettingsPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The full settings modal, loaded from a redacted config. */
export const Default: Story = {};

/** After a save that touched a restart-required setting — shows the restart banner. */
export const RestartRequired: Story = {
  args: { putConfig: putConfig(true) },
};

/** First-run setup: subnet is still editable (not yet baked in). */
export const PreSetup: Story = {
  args: { getConfig: fn(async () => makeAppConfig({ setupComplete: false })) },
};

/** Nothing imported and no groups configured — both account lists and both group editors
 *  show their empty states (and the group editors prompt to import accounts first). */
export const NoAccounts: Story = {
  args: {
    accounts: [],
    getConfig: fn(async () => makeAppConfig({ cloneGroups: [], codexGroups: [] })),
  },
};

/** A self-update in flight: its progress renders inline under the Update button, so the
 *  operator doesn't have to watch the sidebar to know how far along the restart is. */
export const UpdateInProgress: Story = {
  args: { operations: [updateOp] },
};
