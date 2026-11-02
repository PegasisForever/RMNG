import type { Meta, StoryObj } from "@storybook/react-vite";

import { FakeDesktop } from "~/desk/__fixtures__/desktop";
import { PortForwardModal } from "~/components/PortForwardModal";
import type { Clone } from "~/lib/types";

const clone: Clone = {
  id: "pega-abc",
  host: "pega-abc",
  managed: true,
};

const meta: Meta<typeof PortForwardModal> = {
  title: "Modals/Components/PortForwardModal",
  component: PortForwardModal,
  parameters: { layout: "fullscreen" },
  // Over a clone's screen, as the dialog opens
  decorators: [
    (Story) => (
      <div className="relative h-screen w-screen overflow-hidden bg-black">
        <FakeDesktop />
        <Story />
      </div>
    ),
  ],
  args: {
    clone,
    sshPublicHost: "rmng.example.ts.net",
    onClose: () => {},
    onCopy: () => Promise.resolve(true),
  },
};
export default meta;

type Story = StoryObj<typeof PortForwardModal>;
export const Default: Story = {};
