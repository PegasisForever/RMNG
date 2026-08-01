import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SetupServerStep } from "./SetupServerStep";
import { makeSetupDraft } from "./__fixtures__/appConfig";

/** The step sits in the wizard's card, so the story gives it that column width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** The server half of the wizard's form, straight off the seeded draft and rebuilt per story:
 *  the monitors array is what the editor replaces on every edit. */
function base() {
  const draft = makeSetupDraft();
  return {
    hostnamePrefix: draft.hostnamePrefix,
    cloneCpus: draft.cloneCpus,
    cloneMemoryMb: draft.cloneMemoryMb,
    monitors: draft.monitors,
    chroma: draft.chroma,
    listen: draft.listen,
    agentPort: draft.agentPort,
  };
}

const meta = {
  title: "Setup/Components/SetupServerStep",
  component: SetupServerStep,
  parameters: { layout: "centered" },
  args: {
    ...base(),
    portsOpen: false,
    onHostnamePrefixChange: fn(),
    onCloneCpusChange: fn(),
    onCloneMemoryMbChange: fn(),
    onMonitorsChange: fn(),
    onChromaChange: fn(),
    onListenChange: fn(),
    onAgentPortChange: fn(),
    onPortsOpenChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SetupServerStep {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SetupServerStep>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The normal first run: a two-monitor arrangement carried over from the config, and the
 *  ports folded away because almost nobody changes them. */
export const Default: Story = { args: { ...base() } };

/** Ports expanded. Four numbers that make the server unreachable if they are wrong, which is
 *  why they are behind a click. */
export const PortsOpen: Story = {
  args: { ...base(), portsOpen: true },
};

/** No prefix typed, so the example hostname under the field falls back to the one the server
 *  would use. */
export const BlankPrefix: Story = {
  args: { ...base(), hostnamePrefix: "" },
};

/** A host with one screen. The preview draws a single box and Remove is dead: an arrangement
 *  with no monitors is not one a clone can boot. */
export const SingleMonitor: Story = {
  args: {
    ...base(),
    monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
  },
};
