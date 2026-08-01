import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { ClaudeAccountsPanel } from "./ClaudeAccountsPanel";
import {
  accountsNow,
  makeClaudeAccounts,
  makeCloneGroups,
  makeCodexGroups,
  makeUsage,
} from "./__fixtures__/accounts";

/** The panel lives in the board's rail, which is 16rem wide. Every bar and every truncated
 *  email is drawn against that width, so the story gives it the same one. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-64">{children}</div>;
}

const meta = {
  title: "Dashboard/Components/ClaudeAccountsPanel",
  component: ClaudeAccountsPanel,
  parameters: { layout: "centered" },
  args: {
    accounts: makeClaudeAccounts(accountsNow),
    // Nothing dragged: the rows keep the order they arrive in.
    accountOrder: {},
    cloneGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    locale: "en-GB",
    // The same instant the account fixtures are anchored to, so every pace marker and reset
    // countdown is reproducible. The container reads a ticking clock here.
    now: accountsNow,
    onRefresh: fn(),
    onImport: fn(),
  },
  render: (args) => (
    <Frame>
      <ClaudeAccountsPanel {...args} />
    </Frame>
  ),
} satisfies Meta<typeof ClaudeAccountsPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The usual view: each configured pool as its own heading, then whatever no pool claims.
 *  An account in two pools is listed under both, because the pools are how a clone's binding
 *  is resolved. */
export const Default: Story = {};

/** No pools configured, so there is nothing to group by and the list stays flat. */
export const NoPools: Story = {
  args: { cloneGroups: [], codexGroups: [] },
};

/** The order the operator dragged out in Settings, applied. Reversing the Claude bucket moves
 *  those two rows and leaves the Codex row exactly where it was: the order is per provider,
 *  and the two pools are independent. */
export const Reordered: Story = {
  args: {
    accountOrder: { claude: ["claude|sam@example.com", "claude|alex@example.com"] },
  },
};

/** Before the container's clock has ticked. Every bar keeps its fill and drops its pace
 *  marker and its reset tooltip, which is the one frame a prerendered pass can commit to
 *  without guessing the viewer's time zone. */
export const NoClock: Story = {
  args: { now: null },
};

/** A pool with nobody in it, and an account whose usage the poller could not read. Both are
 *  misconfigurations worth seeing: the empty pool leaves every clone bound to it unassigned,
 *  and the unreadable account is the one that will not take work. */
export const ProblemStates: Story = {
  args: {
    accounts: [
      makeUsage({ email: "alex@example.com", fiveHour: { pct: 42, resetsAt: null } }),
      makeUsage({ email: "broken@example.com", error: "401 from the usage endpoint" }),
    ],
    cloneGroups: [{ name: "pooled", accounts: [] }],
    codexGroups: [],
  },
};

/** Nothing imported yet. The whole panel collapses to the one action that gets it started. */
export const NoAccounts: Story = {
  args: { accounts: [], cloneGroups: [], codexGroups: [] },
};
