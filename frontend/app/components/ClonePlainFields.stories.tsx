import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { ClonePlainFields } from "./ClonePlainFields";
import { makeClonePresets } from "./__fixtures__/presets";

/** The fields sit in the dialog's body, so the story gives them the same width. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

const presets = makeClonePresets();

const meta = {
  title: "Clone/Components/ClonePlainFields",
  component: ClonePlainFields,
  parameters: { layout: "centered" },
  args: {
    title: "",
    message: "",
    presets,
    preset: presets[0].name,
    onTitleChange: fn(),
    onMessageChange: fn(),
    onPresetChange: fn(),
    onSubmit: fn(),
  },
  render: (args) => (
    <Frame>
      <ClonePlainFields {...args} />
    </Frame>
  ),
} satisfies Meta<typeof ClonePlainFields>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The tab as it opens: an empty title, no first message, and the first preset picked for the
 *  operator. This is the one tab that picks a preset by hand, because there is no ticket id
 *  and no team key for one to be derived from. */
export const Default: Story = {};

/** A titled scratch box with a first turn queued. The message is sent to the agent the moment
 *  the clone comes up; leaving it empty is what makes a clone start idle. */
export const Filled: Story = {
  args: {
    title: "encoder-scratch",
    message: "Read the VA-API notes in the shared memory, then summarize the options.",
    preset: presets[1].name,
  },
};

/** No presets configured. The control disappears rather than offering an empty dropdown, and
 *  the server falls back on its own. */
export const NoPresets: Story = {
  args: { presets: [], preset: "" },
};
