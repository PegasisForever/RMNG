import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneAccountFields } from "./CloneAccountFields";
import {
  accountsNow,
  makeClaudeAccounts,
  makeGroups,
} from "./__fixtures__/accounts";

/** The pickers sit in the dialog's body, so the story gives them the same width. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

/** The account pool, rebuilt per story. Nothing here copies these into state today, but a
 *  builder called once at module load is the shape that starts leaking the moment something
 *  does, so each story gets its own. */
function pools() {
  return {
    accounts: makeClaudeAccounts(accountsNow),
    groups: makeGroups(),
    group: "",
    onGroupChange: fn(),
  };
}

const meta = {
  title: "Clone/Components/CloneAccountFields",
  component: CloneAccountFields,
  parameters: { layout: "centered" },
  args: {
    ...pools(),
    claudeAccount: "",
    codexAccount: "",
    onClaudeAccountChange: fn(),
    onCodexAccountChange: fn(),
  },
  render: (args) => (
    <Frame>
      <CloneAccountFields {...args} />
    </Frame>
  ),
} satisfies Meta<typeof CloneAccountFields>;

export default meta;
type Story = StoryObj<typeof meta>;

/** No preset has resolved yet, which is every moment before a ticket parses (or a preset
 *  is picked). The group box is blank; both sides read Follow group. */
export const Automatic: Story = { args: { ...pools() } };

/** A resolved preset fills the group box directly with its pool; both sides stay on
 *  Follow group until the operator pins an account. */
export const FilledFromPreset: Story = {
  args: {
    ...pools(),
    group: "team",
    claudeAccount: "",
    codexAccount: "",
  },
};

/** Overridden by hand: this clone draws from one pool and pins its Claude side to one
 *  account, and it stays there whatever preset resolves later. */
export const Overridden: Story = {
  args: { ...pools(), group: "team", claudeAccount: "sam@example.com" },
};

/** Nothing imported and no pools configured. The group box still offers the any-group
 *  escape hatch, and the sides read Follow group. */
export const NothingConfigured: Story = {
  args: {
    accounts: [],
    groups: [],
    group: "",
    claudeAccount: "",
    codexAccount: "",
  },
};
