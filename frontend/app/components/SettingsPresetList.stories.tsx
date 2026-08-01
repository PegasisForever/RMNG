import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SettingsPresetList } from "./SettingsPresetList";
import { accountsNow, makeClaudeAccounts } from "./__fixtures__/accounts";
import { makeSettingsDraft } from "./__fixtures__/appConfig";
import { newPreset } from "~/lib/settingsDraft";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** The three configured presets plus the pools their account pickers offer, rebuilt per
 *  story. Every card here is editable, so one set behind the stories would let an edit in one
 *  show up in the next. */
function base() {
  const draft = makeSettingsDraft();
  return {
    presets: draft.presets,
    accounts: makeClaudeAccounts(accountsNow),
    claudeGroups: draft.claudeGroups,
    codexGroups: draft.codexGroups,
  };
}

const meta = {
  title: "Settings/Components/SettingsPresetList",
  component: SettingsPresetList,
  parameters: { layout: "centered" },
  args: {
    ...base(),
    onChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsPresetList {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsPresetList>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The configured presets. `webapp` claims two team keys and defaults its clones to a pool;
 *  `platform` has no Linear key, which is what its unset badge says and what blocks a clone
 *  from opening a ticket under OPS. */
export const Default: Story = { args: { ...base() } };

/** A preset being filled in. Everything is blank, including both account defaults: a new
 *  preset takes no opinion on which pool its clones get until it is given one. */
export const NewRow: Story = {
  args: { ...base(), presets: [newPreset()] },
};

/** Nothing configured. Cloning from a ticket has no preset to auto-select, so every clone
 *  needs a preset added here first. */
export const Empty: Story = {
  args: { ...base(), presets: [] },
};

/** No accounts imported and no pools configured, so both pickers fall back to the two options
 *  that never depend on config: rotate over everything, or install no token at all. */
export const NothingToDefaultTo: Story = {
  args: { ...base(), accounts: [], claudeGroups: [], codexGroups: [] },
};

/** Wired to local state: renaming, adding a variable, dropping a preset and picking a
 *  default all take effect. */
export const Interactive: Story = {
  args: { ...base() },
  render: function Render(args) {
    const [presets, setPresets] = useState(args.presets);
    return (
      <Frame>
        <SettingsPresetList
          {...args}
          presets={presets}
          onChange={(next) => {
            setPresets(next);
            args.onChange(next);
          }}
        />
      </Frame>
    );
  },
};
