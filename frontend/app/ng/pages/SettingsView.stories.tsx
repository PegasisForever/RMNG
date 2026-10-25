import type { Meta, StoryObj } from "@storybook/react-vite";
import { useArgs } from "storybook/preview-api";
import { fn } from "storybook/test";

import { accountsNow, makeClaudeAccounts } from "~/components/__fixtures__/accounts";
import { makeSettingsDraft, makeUpdateStatus } from "~/components/__fixtures__/appConfig";
import { makeBoardColumns } from "~/components/__fixtures__/board";

import { inPageFrame } from "./__fixtures__/frame";
import { SettingsView, type SettingsViewProps } from "./SettingsView";

const meta = {
  title: "ng/Pages/SettingsView",
  component: SettingsView,
  decorators: [inPageFrame],
  parameters: { layout: "centered" },
  args: {
    draft: makeSettingsDraft(),
    onDraftChange: fn(),
    category: "board",
    onCategoryChange: fn(),
    accounts: makeClaudeAccounts(accountsNow),
    accountOrder: {},
    onImportAccount: fn(),
    forkSources: ["pega-we-142", "pega-dev-88"],
    error: null,
    saving: false,
    saved: false,
    onSave: fn(),
    serverStatus: makeUpdateStatus(),
    serverMessage: null,
    updateOperation: null,
    updateDisabled: true,
    onCheckUpdate: fn(),
    onUpdateServer: fn(),
    onRestartServer: fn(),
    judgeTestMessage: null,
    onTestJudge: fn(),
    boardColumns: makeBoardColumns(),
    boardColumnCounts: { todo: 2, doing: 2, blocked: 1, archived: 0 },
    onAddBoardColumn: fn(),
    onRenameBoardColumn: fn(),
    onSetBoardColumnArchive: fn(),
    onDeleteBoardColumn: fn(),
    onReorderBoardColumns: fn(),
  },
  // The categories switch in every story
  render: function Render(args) {
    const [{ category }, updateArgs] = useArgs<SettingsViewProps>();
    return <SettingsView {...args} category={category} onCategoryChange={(c) => updateArgs({ category: c })} />;
  },
} satisfies Meta<typeof SettingsView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The Board category: the columns. */
export const Default: Story = {};

/** The LLM category: accounts, pools and the activity judge. */
export const Llm: Story = { args: { category: "llm" } };

/** The Server category. */
export const Server: Story = { args: { category: "server" } };

/** Just saved. */
export const Saved: Story = { args: { saved: true } };

/** The save failed. */
export const SaveError: Story = { args: { error: "config: the preset name \"dev\" is used twice" } };

/** The config is still loading. */
export const Loading: Story = { args: { draft: null } };
