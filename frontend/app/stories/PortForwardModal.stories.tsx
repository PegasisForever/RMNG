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
  forwards: [
    { id: "f8080", remotePort: 3000, localPort: 8080, enabled: true, label: "dev server" },
    { id: "f5433", remotePort: 5432, localPort: 5433, enabled: false, label: null },
  ],
};

const meta: Meta<typeof PortForwardModal> = {
  title: "Modals/PortForwardModal",
  component: PortForwardModal,
  args: {
    clone,
    runtime: [
      { id: "f8080", state: "listening", error: null, activeConns: 2 },
      { id: "f5433", state: "error", error: "127.0.0.1:5433: address already in use", activeConns: 0 },
    ],
    busy: false,
    error: null,
    onClose: () => {},
    onSubmit: () => {},
  },
};
export default meta;

type Story = StoryObj<typeof PortForwardModal>;
export const Default: Story = {};
export const WithError: Story = { args: { error: "local port 8080 is already in use" } };
