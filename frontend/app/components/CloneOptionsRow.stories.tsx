import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneOptionsRow } from "./CloneOptionsRow";

/** The row sits in the dialog's body, so the story gives it the same width. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

const meta = {
  title: "Clone/Components/CloneOptionsRow",
  component: CloneOptionsRow,
  parameters: { layout: "centered" },
  args: {
    headless: false,
    onHeadlessChange: fn(),
  },
  render: (args) => (
    <Frame>
      <CloneOptionsRow {...args} />
    </Frame>
  ),
} satisfies Meta<typeof CloneOptionsRow>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Headless off: the viewer shows a video stream. */
export const Headed: Story = {};

/** Headless on: no desktop, so the viewer shows a tmux tab view instead of a stream. */
export const Headless: Story = {
  args: { headless: true },
};
