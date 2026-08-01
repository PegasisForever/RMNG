import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SettingsProviderFields } from "./SettingsProviderFields";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const meta = {
  title: "Settings/Components/SettingsProviderFields",
  component: SettingsProviderFields,
  parameters: { layout: "centered" },
  args: {
    pollSecs: 600,
    pinnedEmail: "alex@example.com",
    onPollSecsChange: fn(),
    onPinnedEmailChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsProviderFields {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsProviderFields>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The Claude section: two fields and nothing else. */
export const Claude: Story = {};

/** Nobody pinned, which is the usual state — the usage list then sorts itself. */
export const NoPinnedAccount: Story = {
  args: { pinnedEmail: "" },
};

/** The Codex section: the same two fields plus the two switches only Codex has. Usage polling
 *  is the escape hatch for when the unofficial usage endpoint drifts, and auto-reset spends a
 *  banked credit when every account is over its weekly cap. */
export const Codex: Story = {
  args: {
    pinnedEmail: "",
    codexToggles: {
      usagePolling: true,
      autoReset: false,
      onUsagePollingChange: fn(),
      onAutoResetChange: fn(),
    },
  },
};

/** Codex with usage polling off and auto-reset on, so both switches read the other way. */
export const CodexSwitched: Story = {
  args: {
    pinnedEmail: "alex@openai.com",
    codexToggles: {
      usagePolling: false,
      autoReset: true,
      onUsagePollingChange: fn(),
      onAutoResetChange: fn(),
    },
  },
};
