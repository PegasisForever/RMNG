import { DndContext } from "@dnd-kit/core";
import type { Meta, StoryObj } from "@storybook/react-vite";

import { TicketColumn } from "./TicketColumn";
import { openTickets, orderTickets } from "~/lib/tickets";
import { hosts } from "./__fixtures__/clones";
import { linearTickets } from "./__fixtures__/tickets";

const meta = {
  title: "Board/TicketColumn",
  component: TicketColumn,
  parameters: { layout: "fullscreen" },
  args: {
    tickets: openTickets(linearTickets, hosts),
    loading: false,
    error: null,
  },
  /** The cards are draggable, so they need a DndContext even when nothing can receive
   *  them. On the board that context is the board's own. */
  render: (args) => (
    <DndContext>
      <div className="flex h-screen bg-slate-50 p-3 dark:bg-slate-950">
        <TicketColumn {...args} />
      </div>
    </DndContext>
  ),
} satisfies Meta<typeof TicketColumn>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Four open tickets, in Linear's order because nobody has rearranged the column yet. The
 *  fixture holds seven rows: `WE-142` and `DEV-88` already have clones, and `DEV-104` is in
 *  there twice as two presets sharing a key would return it. `openTickets` drops all three. */
export const Default: Story = {};

/** The column after the operator has moved things around. `DEV-97` was dragged to the top,
 *  and `WE-301` arrived from Linear afterwards, so it sits above the arrangement rather than
 *  under it: new work goes where it will be seen. */
export const OperatorOrder: Story = {
  args: {
    tickets: orderTickets(openTickets(linearTickets, hosts), ["DEV-97", "WE-288", "DEV-104"]),
  },
};

/** Everything open has been started already. Not an error state, so it reads as a finished
 *  queue rather than a broken one. */
export const AllClaimed: Story = {
  args: { tickets: [] },
};

/** Before the first list arrives. The count reads as a placeholder rather than zero, and
 *  the empty state stays quiet: nothing has been said yet, so nothing is claimed. */
export const FirstLoad: Story = {
  args: { tickets: [], loading: true },
};

/** The server's poll of Linear failed. The column keeps whatever it last had and says what
 *  happened, since a stale list beats an empty one when the fleet is mid-flight. */
export const PollFailed: Story = {
  args: { error: "linear: 401 Unauthorized (check the preset's API key)" },
};

/** No key configured and nothing cached: the empty state has to carry the message alone. */
export const FailedAndEmpty: Story = {
  args: { tickets: [], error: "linear: no API key on any preset" },
};
