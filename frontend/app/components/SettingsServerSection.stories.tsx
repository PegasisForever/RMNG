import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SettingsServerSection } from "./SettingsServerSection";
import { makeUpdateStatus } from "./__fixtures__/appConfig";
import { makeOperation } from "./__fixtures__/operations";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const meta = {
  title: "Settings/Components/SettingsServerSection",
  component: SettingsServerSection,
  parameters: { layout: "centered" },
  args: {
    status: makeUpdateStatus(),
    message: null,
    operation: null,
    updateDisabled: true,
    onCheckUpdate: fn(),
    onUpdate: fn(),
    onRestart: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsServerSection {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsServerSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Running the newest published image. Update is dead because there is nothing to move to. */
export const UpToDate: Story = {};

/** A newer image exists. The badge turns amber and Update comes alive. */
export const UpdateAvailable: Story = {
  args: {
    status: makeUpdateStatus({
      available: true,
      remoteDigest: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
    }),
    message: "update available",
    updateDisabled: false,
  },
};

/** The update is running. Its progress renders here rather than only in the rail, because the
 *  server restarts itself partway through and this is the panel that started it. */
export const Updating: Story = {
  args: {
    status: makeUpdateStatus({ available: true }),
    message: "updating… the server will restart shortly",
    operation: makeOperation({
      id: "op_update_1",
      kind: "update",
      target: "rmng-control",
      step: "pull",
      pct: 45,
      message: "pulling pegasis0/rmng:latest",
      log: ["queued self-update", "pulling pegasis0/rmng:latest"],
    }),
  },
};

/** The check itself failed — the registry was unreachable — so the panel says what it knows
 *  and leaves the running version standing. */
export const CheckFailed: Story = {
  args: {
    status: makeUpdateStatus({ remoteDigest: null, error: "registry unreachable" }),
    message: "⚠ registry unreachable",
  },
};

/** A dev build: no version labels on the running image, so there is nothing to compare and no
 *  badge to draw. */
export const DevBuild: Story = {
  args: {
    status: null,
    message: null,
  },
};
