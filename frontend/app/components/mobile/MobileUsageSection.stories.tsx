import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { MobileUsageSection } from "./MobileUsageSection";
import {
  accountsNow,
  makeClaudeAccounts,
  makeCloneGroups,
  makeCodexGroups,
  makeUsage,
} from "../__fixtures__/accounts";
import { makeStoryLink } from "../__fixtures__/storyLinks";

/** The section spans a phone's width, so the story gives it one. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[390px] bg-slate-100 dark:bg-slate-950">{children}</div>;
}

/** The account rows and the pools, rebuilt per story: the panel inside holds them. */
function base() {
  return {
    accounts: makeClaudeAccounts(accountsNow),
    accountOrder: {},
    cloneGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
  };
}

const meta = {
  title: "Mobile/Components/MobileUsageSection",
  component: MobileUsageSection,
  parameters: { layout: "centered" },
  args: {
    ...base(),
    locale: "en-GB",
    // The container's clock, pinned to the instant the account fixtures are anchored to.
    now: accountsNow,
    usageOpen: false,
    onUsageOpenChange: fn(),
    onRefresh: fn(),
    // Importing opens a dialog the container mounts, so the story jumps to that dialog's
    // own story rather than stacking it on this section.
    onImportAccount: makeStoryLink("Settings/Components/ImportAccountModalView", "SignedIn"),
  },
  render: (args) => (
    <Frame>
      <MobileUsageSection {...args} />
    </Frame>
  ),
} satisfies Meta<typeof MobileUsageSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Folded, which is how the home screen opens. The line reports the worst gating window
 *  anyone is at and who is at it: 88% of a 5h limit, on sam@example.com. */
export const Collapsed: Story = { args: { ...base() } };

/** Unfolded, on the desktop's own account panel. It costs most of a phone screen, which is
 *  the reason the fold exists. */
export const Expanded: Story = {
  args: { ...base(), usageOpen: true },
};

/** Accounts imported, but the poller has not filled a window in yet. There is no number to
 *  report, so the line says so rather than showing a zero nobody should act on. */
export const NoData: Story = {
  args: {
    ...base(),
    accounts: [makeUsage({ email: "alex@example.com" }), makeUsage({ email: "sam@example.com" })],
  },
};

/** Nothing imported. The fold still opens, onto the panel's own empty state and the one
 *  action that gets it started. */
export const NoAccounts: Story = {
  args: { ...base(), accounts: [], cloneGroups: [], codexGroups: [], usageOpen: true },
};

/** Before the container's clock has ticked. Every bar keeps its fill and drops its pace
 *  marker and its reset tooltip, which is the one frame a prerendered pass can commit to. */
export const NoClock: Story = {
  args: { ...base(), now: null, usageOpen: true },
};

/** Wired to local state, so the fold really folds. */
export const Interactive: Story = {
  args: { ...base() },
  render: function Render(args) {
    const [usageOpen, setUsageOpen] = useState(args.usageOpen);
    return (
      <Frame>
        <MobileUsageSection
          {...args}
          usageOpen={usageOpen}
          onUsageOpenChange={(open) => {
            setUsageOpen(open);
            args.onUsageOpenChange(open);
          }}
        />
      </Frame>
    );
  },
};
