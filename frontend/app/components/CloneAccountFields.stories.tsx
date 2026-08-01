import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneAccountFields } from "./CloneAccountFields";
import {
  accountsNow,
  makeClaudeAccounts,
  makeCloneGroups,
  makeCodexGroups,
} from "./__fixtures__/accounts";
import { makeClonePresets, makePreset } from "./__fixtures__/presets";

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
    claudeGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
  };
}

const meta = {
  title: "Clone/Components/CloneAccountFields",
  component: CloneAccountFields,
  parameters: { layout: "centered" },
  args: {
    ...pools(),
    preset: makeClonePresets()[0],
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
 *  the preset", and the option says which pool or account that is when the preset names one.
 *  `webapp` names a Claude pool, so the Claude picker reads "Preset default (group:pooled)".
 *  It names no Codex account, so the Codex picker falls back to the generic label — which is
 *  the usual case, a Codex default being optional. */
export const PresetDefaults: Story = { args: { ...pools() } };

/** The same shape on the other side: a preset that names a Codex pool and no Claude account,
 *  so the two labels swap over. Built here rather than added to the shared preset fixture,
 *  because those three also drive the ticket dialog's team dropdown. */
export const CodexPresetDefault: Story = {
  args: {
    ...pools(),
    preset: makePreset({ name: "codex-first", labels: ["CX"], codexAccount: "group:team" }),
  },
};

/** A preset that names no defaults. Both blank options fall back to the generic label, and
 *  picking nothing leaves the server to resolve the pool from its own chain. */
export const NoPresetDefaults: Story = {
  args: { ...pools(), preset: makeClonePresets()[1] },
};

/** No preset has resolved yet, which is every moment before a ticket parses. Same generic
 *  labels, for a different reason. */
export const NoPreset: Story = {
  args: { ...pools(), preset: undefined },
};

/** Overridden by hand: this clone is pinned to one Claude account and one Codex pool, and it
 *  stays there whatever preset it ends up on. */
export const Overridden: Story = {
  args: { ...pools(), claudeAccount: "sam@example.com", codexAccount: "group:team" },
};

/** Nothing imported and no pools configured. Both pickers fall back to the two options that
 *  never depend on config: rotate over everything, or install no token at all. */
export const NothingConfigured: Story = {
  args: { accounts: [], claudeGroups: [], codexGroups: [], preset: undefined },
};
