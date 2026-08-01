import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SettingsLayoutPresets } from "./SettingsLayoutPresets";
import { makeSettingsDraft } from "./__fixtures__/appConfig";
import { newLayoutPreset } from "~/lib/settingsDraft";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const meta = {
  title: "Settings/Components/SettingsLayoutPresets",
  component: SettingsLayoutPresets,
  parameters: { layout: "centered" },
  args: {
    presets: makeSettingsDraft().layoutPresets,
    onChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsLayoutPresets {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsLayoutPresets>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One configured arrangement: a 1440p primary with a 1080p screen to its right, drawn to
 *  scale above the numbers that produced it. */
export const Default: Story = { args: { presets: makeSettingsDraft().layoutPresets } };

/** Several arrangements, which is the shape a rig gets once it has one layout for coding and
 *  another for reviewing. The active one is switched from the rail, not here. */
export const Several: Story = {
  args: {
    presets: [
      ...makeSettingsDraft().layoutPresets,
      { name: "Single 1080p", monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }] },
      newLayoutPreset("Unnamed placeholder"),
    ],
  },
};

/** Nothing configured. Reachable only by removing every preset, because a config with none
 *  is seeded with one to edit rather than left empty. */
export const Empty: Story = {
  args: { presets: [] },
};

/** Wired to local state: adding, renaming, dropping a preset and dragging its monitors all
 *  take effect. */
export const Interactive: Story = {
  render: function Render(args) {
    const [presets, setPresets] = useState(args.presets);
    return (
      <Frame>
        <SettingsLayoutPresets
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
