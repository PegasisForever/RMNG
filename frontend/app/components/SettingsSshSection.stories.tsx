import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SettingsSshSection } from "./SettingsSshSection";
import { makeSettingsDraft } from "./__fixtures__/appConfig";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const draft = makeSettingsDraft();

const meta = {
  title: "Settings/Components/SettingsSshSection",
  component: SettingsSshSection,
  parameters: { layout: "centered" },
  args: {
    authorizedKeys: draft.ssh.authorizedKeys,
    publicHost: draft.ssh.publicHost,
    bastionPort: draft.listen.bastion,
    onAuthorizedKeysChange: fn(),
    onPublicHostChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsSshSection {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsSshSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One key installed and a host named, which is what makes the per-clone Copy SSH command
 *  button produce something that works from a laptop. */
export const Default: Story = {};

/** No keys pasted, so nothing can reach the bastion and the copied command would fail at the
 *  jump. The host override is blank too, which means the page's own address is used. */
export const NoKeys: Story = {
  args: { authorizedKeys: [], publicHost: "" },
};

/** Several keys, one per line, which is how a second laptop or a CI runner gets in. */
export const SeveralKeys: Story = {
  args: {
    authorizedKeys: [
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeStorybookDemoKeyOnly alex@laptop",
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeStorybookDemoKeyTwo alex@desktop",
      "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQFakeStorybookDemoKeyThree ci@runner",
    ],
  },
};
