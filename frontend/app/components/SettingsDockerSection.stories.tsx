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
    testMessage: null,
    onHostnamePrefixChange: fn(),
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

/** The fleet's Docker defaults: hostname prefix plus per-clone sizing. */
export const Default: Story = {};

/** The Docker probe answered. The same line carries the failure, prefixed with ✗ instead. */
export const Probed: Story = {
  args: { testMessage: "✓ Docker reachable (Engine 27.1.1)" },
};

/** Nothing typed into the prefix, so the example hostnames fall back to the default the
 *  server would use. */
export const BlankPrefix: Story = {
  args: { hostnamePrefix: "" },
};
