import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { accountsNow, makeClaudeAccounts, makeGroups, makeUsage } from "~/components/__fixtures__/accounts";
import { cloneOperation, deleteOperation } from "~/components/__fixtures__/operations";

import { inPageFrame } from "./__fixtures__/frame";
import { UsageView } from "./UsageView";

const meta = {
  title: "ng/Control panel/UsageView",
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

/** The clone's sound plays: its line at the bottom. */
export const WithSound: Story = {
  args: {
    sound: { target: 6, late: 3, dropped: 1, sampleRate: 48000, baseLatency: 0.005, outputLatency: 0.012 },
  },
};

/** No accounts imported yet. */
export const NoAccounts: Story = { args: { accounts: [] } };

/** An account whose refresh token the provider rejected, with no numbers: only its sign-in
 *  button, not the server's error. */
export const SignedOut: Story = {
  args: {
    groups: [],
    accounts: [
      makeUsage({
        email: "talktomedi.claude.dev.0@gmail.com",
        assignable: false,
        error:
          "talktomedi.claude.dev.0@gmail.com's Claude refresh token was rejected by the provider, so no refresh is attempted until it is signed in again: refresh 400",
      }),
      ...makeClaudeAccounts(accountsNow),
    ],
  },
};

/** The rail (wide, a mouse): one column; the reset times are tooltips, and the scroll has no bar. */
export const Rail: Story = { args: { rail: true }, parameters: { frameWidth: 256 } };
