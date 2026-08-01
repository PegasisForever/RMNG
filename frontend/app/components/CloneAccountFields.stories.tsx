import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneAccountFields } from "./CloneAccountFields";
import { makeClaudeAccounts, makeCloneGroups, makeCodexGroups } from "./__fixtures__/accounts";
import { makeClonePresets } from "./__fixtures__/presets";

/** The pickers sit in the dialog's body, so the story gives them the same width. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

const presets = makeClonePresets();

const meta = {
  title: "Clone/Components/CloneAccountFields",
  component: CloneAccountFields,
  parameters: { layout: "centered" },
  args: {
    accounts: makeClaudeAccounts(),
    claudeGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    preset: presets[0],
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

/** Both pickers on their blank option, which is the state that matters: blank means "follow
 *  the preset", and the option says which pool or account that is, so the operator can see
 *  what an override would be overriding. */
export const PresetDefaults: Story = {};

/** A preset that names no defaults. The blank option falls back to the generic label, and
 *  picking nothing leaves the server to resolve the pool from its own chain. */
export const NoPresetDefaults: Story = {
  args: { preset: presets[1] },
};

/** No preset has resolved yet, which is every moment before a ticket parses. Same generic
 *  labels, for a different reason. */
export const NoPreset: Story = {
  args: { preset: undefined },
};

/** Overridden by hand: this clone is pinned to one Claude account and one Codex pool, and it
 *  stays there whatever preset it ends up on. */
export const Overridden: Story = {
  args: { claudeAccount: "sam@example.com", codexAccount: "group:team" },
};

/** Nothing imported and no pools configured. Both pickers fall back to the two options that
 *  never depend on config: rotate over everything, or install no token at all. */
export const NothingConfigured: Story = {
  args: { accounts: [], claudeGroups: [], codexGroups: [], preset: undefined },
};
