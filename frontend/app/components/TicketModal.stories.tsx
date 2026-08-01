import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { TicketModal } from "./TicketModal";
import { presets } from "~/stories/fixtures";

/** Resolves after a beat, so the button's "Creating…" state is visible in the story. */
const slowCreate = fn(
  (t: unknown) => new Promise((resolve) => setTimeout(() => resolve(t), 800)),
);

const meta = {
  title: "Board/TicketModal",
  component: TicketModal,
  parameters: { layout: "fullscreen" },
  args: {
    presets,
    onClose: fn(),
    onCreate: slowCreate,
  },
  /** The dialog positions itself over the page, so the story gives it a board-coloured one
   *  to sit on rather than a white void. */
  render: (args) => (
    <div className="h-screen bg-slate-50 dark:bg-slate-950">
      <TicketModal {...args} />
    </div>
  ),
} satisfies Meta<typeof TicketModal>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The normal case: WE is the first team key, and its preset holds a Linear key. */
export const Default: Story = {};

/** The chosen team's preset carries no Linear key, so nothing can open a ticket there. The
 *  dialog says which preset and where to fix it, and the create button stays down. Pick OPS
 *  in the dropdown to see it. */
export const NoLinearKey: Story = {
  args: { presets: presets.filter((p) => !p.linearKeySet) },
};

/** No preset declares a team key at all, which is the first-run state. */
export const NoTeams: Story = {
  args: { presets: [] },
};

/** Linear refused it. The dialog keeps everything typed and shows what came back. */
export const CreateFails: Story = {
  args: {
    onCreate: fn((_t: unknown) =>
      Promise.reject(new Error("Linear refused a new ticket in WE")),
    ),
  },
};
