import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SETTINGS_CATEGORIES, SettingsNav, type SettingsCategory } from "./SettingsNav";

const meta = {
  title: "Settings/Components/SettingsNav",
  component: SettingsNav,
  args: {
    categories: SETTINGS_CATEGORIES,
    active: "board",
    onSelect: fn(),
  },
  // The rail is a column inside the panel, so it renders against the panel's own surface at
  // roughly the width it gets there.
  decorators: [
    (Story) => (
      <div className="w-56 border-r border-slate-100 bg-white p-1 dark:border-slate-800 dark:bg-slate-800">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof SettingsNav>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Every category, sitting on the first. */
export const Default: Story = {};

/** Partway down the rail, which is where the active fill has to read as the current pane
 *  rather than as a hover. */
export const MidList: Story = { args: { active: "claude" } };

/** A page with no board drops that category. The rail is a filter of one list, so nothing
 *  else moves. */
export const NoBoard: Story = {
  args: {
    categories: SETTINGS_CATEGORIES.filter((c) => c.id !== "board"),
    active: "agents",
  },
};

/** The rail wired to local state: clicking really moves the fill. */
export const Interactive: Story = {
  render: function Render(args) {
    const [active, setActive] = useState<SettingsCategory>(args.active);
    return (
      <SettingsNav
        {...args}
        active={active}
        onSelect={(next) => {
          setActive(next);
          args.onSelect(next);
        }}
      />
    );
  },
};
