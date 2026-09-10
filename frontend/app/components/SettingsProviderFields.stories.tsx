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
    pinnedEmail: "alex@example.com",
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

/** The Claude section: one field and nothing else. */
export const Claude: Story = {};

/** Nobody pinned, which is the usual state — the usage list then sorts itself. */
export const NoPinnedAccount: Story = {
  args: { pinnedEmail: "" },
};

/** The Codex section: the same field plus the auto-reset switch. */
export const Codex: Story = {
  args: {
    pinnedEmail: "",
    autoReset: { value: false, onChange: fn() },
  },
};

/** Codex with auto-reset on. */
export const CodexSwitched: Story = {
  args: {
    pinnedEmail: "alex@openai.com",
    autoReset: { value: true, onChange: fn() },
  },
};
