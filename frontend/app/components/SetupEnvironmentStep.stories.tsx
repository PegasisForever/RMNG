import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SetupEnvironmentStep } from "./SetupEnvironmentStep";
import { EnvChecklistView } from "./EnvChecklistView";
import { makeEnvCheckRow, makeEnvRows } from "./__fixtures__/setupEnv";

/** The step sits in the wizard's card, so the story gives it that column width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** The checklist as the container mounts it, minus the probe. The step takes it as a slot, so
 *  a story hands over the View directly and picks which host it describes. */
function checklist(rows = makeEnvRows()) {
  return <EnvChecklistView rows={rows} loading={false} error={null} onRetry={fn()} />;
}

const meta = {
  title: "Setup/Components/SetupEnvironmentStep",
  component: SetupEnvironmentStep,
  parameters: { layout: "centered" },
  args: {
    subnet: "10.99.0.0/24",
    envChecklist: checklist(),
    onSubnetChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SetupEnvironmentStep {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SetupEnvironmentStep>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A host that is ready and a subnet the server will accept. This is the only combination
 *  that unlocks the wizard's Next button. */
export const ChecksPassing: Story = {};

/** Docker is unreachable. The checklist says so in red and the wizard cannot advance, however
 *  good the subnet below it is. */
export const ChecksFailing: Story = {
  args: {
    envChecklist: checklist([
      makeEnvCheckRow({ ok: false, detail: "connect /var/run/docker.sock: permission denied" }),
      ...makeEnvRows().slice(1),
    ]),
  },
};

/** A subnet the server would reject: the hint under the field turns red and states the rule.
 *  /8 is the common mistake, and it is too wide for the bridge. */
export const InvalidSubnet: Story = {
  args: { subnet: "10.0.0.0/8" },
};

/** Nothing typed yet. The field keeps its grey explanation of what the addresses are used
 *  for rather than accusing the operator of an error they have not made. */
export const BlankSubnet: Story = {
  args: { subnet: "" },
};
