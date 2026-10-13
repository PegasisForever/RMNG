import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { Field, Secret, Section, settingsInput } from "./SettingsFields";

const meta = {
  title: "Settings/Components/SettingsFields",
  component: Secret,
  parameters: { layout: "centered" },
  args: {
    label: "Linear API key",
    set: true,
    value: "",
    onChange: fn(),
  },
  render: (args) => (
    <div className="w-[36rem] p-4">
      <Secret {...args} />
    </div>
  ),
} satisfies Meta<typeof Secret>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The server holds a key. The input stays blank, meaning "keep it", and the badge says so —
 *  which is the whole point of a write-only field: there is nothing to read back. */
export const SecretSet: Story = {};

/** Nothing stored. The placeholder says "not set" and anything typed becomes the key. */
export const SecretUnset: Story = {
  args: { set: false },
};

/** A key being replaced. The characters are masked, so the badge is still the only thing
 *  saying whether one was there before. */
export const SecretBeingTyped: Story = {
  render: function Render(args) {
    const [value, setValue] = useState("lin_api_pasted_here");
    return (
      <div className="w-[36rem] p-4">
        <Secret {...args} value={value} onChange={setValue} />
      </div>
    );
  },
};
