import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SettingsDockerSection } from "./SettingsDockerSection";
import { makeSettingsDraft } from "./__fixtures__/appConfig";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** The Docker half of the form, straight off the seeded draft. */
function base() {
  const draft = makeSettingsDraft();
  return {
    hostnamePrefix: draft.hostnamePrefix,
    templateReference: draft.templateReference,
    subnet: draft.subnet,
    cloneCpus: draft.cloneCpus,
    cloneMemoryMb: draft.cloneMemoryMb,
  };
}

const meta = {
  title: "Settings/Components/SettingsDockerSection",
  component: SettingsDockerSection,
  parameters: { layout: "centered" },
  args: {
    ...base(),
    subnetLocked: true,
    testMessage: null,
    onHostnamePrefixChange: fn(),
    onTemplateReferenceChange: fn(),
    onSubnetChange: fn(),
    onCloneCpusChange: fn(),
    onCloneMemoryMbChange: fn(),
    onTest: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsDockerSection {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsDockerSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A rig past first-run setup: the subnet is greyed out and its hint says why, because it is
 *  baked into the bridge and every clone's static IP. */
export const Default: Story = {};

/** During first-run setup, and the only moment the subnet is editable. The hint changes to
 *  the rule the value has to satisfy. */
export const PreSetup: Story = {
  args: { subnetLocked: false },
};

/** The Docker probe answered. The same line carries the failure, prefixed with ✗ instead. */
export const Probed: Story = {
  args: { testMessage: "✓ Docker reachable (Engine 27.1.1)" },
};

/** Nothing typed into the prefix, so the example hostnames fall back to the default the
 *  server would use. */
export const BlankPrefix: Story = {
  args: { hostnamePrefix: "" },
};
