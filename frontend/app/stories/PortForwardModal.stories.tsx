import type { Meta, StoryObj } from "@storybook/react-vite";

import { PortForwardModal } from "~/components/PortForwardModal";
import type { Clone } from "~/lib/types";

const clone: Clone = {
  id: "pega-abc",
  host: "pega-abc",
  port: 3389,
  username: "",
  password: "",
  managed: true,
};

const meta: Meta<typeof PortForwardModal> = {
  title: "Modals/Components/PortForwardModal",
  component: PortForwardModal,
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
