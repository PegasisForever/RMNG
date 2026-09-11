import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SettingsPresetList } from "./SettingsPresetList";
import { makeSettingsDraft } from "./__fixtures__/appConfig";
import { newPreset } from "~/lib/settingsDraft";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** The three configured presets, the pools the group picker offers, and the forkable
 *  clones the default-source picker offers, rebuilt per story. Every card here is
 *  editable, so one set behind the stories would let an edit in one show up in the next. */
function base() {
  const draft = makeSettingsDraft();
  return {
    presets: draft.presets,
    groups: draft.groups,
    forkSources: ["pega-we-142", "pega-dev-88", "scratch-box"],
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
 *  `platform` has a blank Linear key, which is what blocks a clone from opening a ticket
 *  under OPS. */
export const Default: Story = { args: { ...base() } };

/** A preset being filled in. It points at the first pool: a preset always names a default. */
export const NewRow: Story = {
  args: { ...base(), presets: [newPreset("pooled")] },
};

/** Nothing configured. Cloning from a ticket has no preset to auto-select, so every clone
 *  needs a preset added here first. */
export const Empty: Story = {
  args: { ...base(), presets: [] },
};

/** No pools configured, so the picker offers only any group. (Unreachable from the
 *  server, which always keeps at least one pool.) */
export const NothingToDefaultTo: Story = {
  args: {
    ...base(),
    groups: [],
    presets: [newPreset("none")],
  },
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
