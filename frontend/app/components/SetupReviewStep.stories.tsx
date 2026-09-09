import type { Meta, StoryObj } from "@storybook/react-vite";

import { SetupReviewStep } from "./SetupReviewStep";
import { makeSetupDraft } from "./__fixtures__/appConfig";

/** The step sits in the wizard's card, so the story gives it that column width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const meta = {
  title: "Setup/Components/SetupReviewStep",
  component: SetupReviewStep,
  parameters: { layout: "centered" },
  args: {
    draft: makeSetupDraft(),
  },
  render: (args) => (
    <Frame>
      <SetupReviewStep {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SetupReviewStep>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A complete first run: every field filled in. */
export const Default: Story = {};

/** The barest rig the wizard will finish: no hostname prefix, one screen, and a subnet field
 *  the operator emptied. The two blanks read differently on purpose — a missing prefix is a
 *  choice, a missing subnet is a gap. */
export const Minimal: Story = {
  args: {
    draft: makeSetupDraft({
      subnet: "",
      hostnamePrefix: "",
      monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
    }),
  },
};
