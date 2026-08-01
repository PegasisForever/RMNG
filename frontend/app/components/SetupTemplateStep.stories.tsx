import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SetupTemplateStep } from "./SetupTemplateStep";
import { makeSetupDraft } from "./__fixtures__/appConfig";
import { makePullOperation } from "./__fixtures__/operations";

/** The step sits in the wizard's card, so the story gives it that column width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const templateRef = makeSetupDraft().templateReference;

const meta = {
  title: "Setup/Components/SetupTemplateStep",
  component: SetupTemplateStep,
  parameters: { layout: "centered" },
  args: {
    templateReference: templateRef,
    placeholder: templateRef,
    operation: null,
    pullTarget: null,
    pulling: false,
    pullRunning: false,
    pullDone: false,
    canPull: true,
    onTemplateReferenceChange: fn(),
    onPull: fn(),
    onSkip: fn(),
  },
  render: (args) => (
    <Frame>
      <SetupTemplateStep {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SetupTemplateStep>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Nothing pulled yet. The reference is editable, Download is live, and Skip is there because
 *  a template can be pulled later from the Images panel. */
export const Default: Story = {};

/** The POST that starts the pull is in flight. The button says what is happening and takes no
 *  second click, but the server has not created the operation yet, so there is no bar. */
export const Starting: Story = {
  args: { pulling: true, canPull: false },
};

/** The pull is running. Six or seven gigabytes over the wire, so the operation drives a real
 *  progress bar; the reference locks and Skip goes away, because leaving now strands the pull. */
export const Pulling: Story = {
  args: {
    operation: makePullOperation(),
    pullTarget: templateRef,
    pullRunning: true,
    canPull: false,
  },
};

/** The template landed. The reference stays locked, the bar reads full and the green line
 *  names what was pulled, which is the same name the Finish step reports. */
export const Pulled: Story = {
  args: {
    operation: makePullOperation({ status: "done", pct: 100, message: "Pulled." }),
    pullTarget: templateRef,
    pullDone: true,
    canPull: false,
  },
};

/** The field cleared out. Download is dead because there is nothing to pull, and the
 *  placeholder shows the configured reference a blank field would fall back to. */
export const BlankReference: Story = {
  args: { templateReference: "", canPull: false },
};
