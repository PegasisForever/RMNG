import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneOptionsRow } from "./CloneOptionsRow";
import { makeCloneWorking } from "./__fixtures__/clones";

/** The row sits in the dialog's body, so the story gives it the same width — which is what
 *  makes the parent's long display name truncate the way it does in the dialog. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

const meta = {
  title: "Clone/Components/CloneOptionsRow",
  component: CloneOptionsRow,
  parameters: { layout: "centered" },
  args: {
    headless: false,
    parentCandidate: null,
    asSubClone: false,
    onHeadlessChange: fn(),
    onAsSubCloneChange: fn(),
  },
  render: (args) => (
    <Frame>
      <CloneOptionsRow {...args} />
    </Frame>
  ),
} satisfies Meta<typeof CloneOptionsRow>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Nothing selected on the board, or the selection cannot parent anything. One switch. */
export const NoParent: Story = {};

/** A managed top-level clone is selected, so the new one can be nested under it. The name is
 *  the parent's, truncated to whatever the row has left. */
export const ParentOffered: Story = {
  args: { parentCandidate: makeCloneWorking() },
};

/** Both switches on: a headless sub clone, which is the usual shape of a helper spawned to
 *  run something for its parent. */
export const HeadlessSubClone: Story = {
  args: { headless: true, parentCandidate: makeCloneWorking(), asSubClone: true },
};
