import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneModeTabs } from "./CloneModeTabs";

/** The strip spans the dialog's body, so the story gives it the same width rather than letting
 *  three buttons size themselves. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

const meta = {
  title: "Clone/Components/CloneModeTabs",
  component: CloneModeTabs,
  parameters: { layout: "centered" },
  args: {
    mode: "existing",
    disabled: false,
    onModeChange: fn(),
  },
  render: (args) => (
    <Frame>
      <CloneModeTabs {...args} />
    </Frame>
  ),
} satisfies Meta<typeof CloneModeTabs>;

export default meta;
type Story = StoryObj<typeof meta>;

/** How the dialog opens: on the tab that clones from a ticket somebody already wrote. */
export const ExistingTicket: Story = {};

/** The tab that opens the Linear issue as part of the clone. */
export const NewTicket: Story = { args: { mode: "create" } };

/** The tab that never touches Linear. */
export const NoTicket: Story = { args: { mode: "plain" } };

/** Locked, which is every tab once a clone has been started. Switching now would rebuild the
 *  form under a request that is already in flight. */
export const Busy: Story = { args: { disabled: true } };
