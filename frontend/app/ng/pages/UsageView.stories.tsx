import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { accountsNow, makeClaudeAccounts, makeGroups } from "~/components/__fixtures__/accounts";
import { cloneOperation, deleteOperation } from "~/components/__fixtures__/operations";

import { inPageFrame } from "./__fixtures__/frame";
import { UsageView } from "./UsageView";

const meta = {
  title: "ng/Pages/UsageView",
  component: UsageView,
  decorators: [inPageFrame],
  parameters: { layout: "centered" },
  args: {
    accounts: makeClaudeAccounts(accountsNow),
    accountOrder: {},
    groups: makeGroups(),
    now: accountsNow,
    operations: [],
    onRefresh: fn(),
    onReplaceAccount: fn(),
  },
} satisfies Meta<typeof UsageView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The accounts in their pools. The Codex one holds no token that works: it asks for a sign-in. */
export const Default: Story = {};

/** No pools: one flat group. */
export const NoPools: Story = { args: { groups: [] } };

/** Jobs running on the server show at the top. */
export const RunningJobs: Story = { args: { operations: [cloneOperation, deleteOperation] } };

/** No accounts imported yet. */
export const NoAccounts: Story = { args: { accounts: [] } };
