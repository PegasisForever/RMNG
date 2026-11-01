import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { hosts } from "~/components/__fixtures__/clones";
import { linearTickets, teamLabels, teamStates, ticketDetailed } from "~/components/__fixtures__/tickets";
import { openTickets } from "~/lib/tickets";
import { linearWorkspaces } from "~/components/__fixtures__/workspaces";
import { TicketPanel } from "~/components/TicketPanel";

import { inPageFrame } from "./__fixtures__/frame";
import { TicketsView } from "./TicketsView";

const meta = {
  title: "ng/Control panel/TicketsView",
  component: TicketsView,
  decorators: [inPageFrame],
  parameters: { layout: "centered" },
  args: {
    // As the container gives them: open, no clone made for them yet, each once
    tickets: openTickets(linearTickets, hosts),
    loading: false,
    error: null,
    open: null,
    onOpen: fn(),
    onBack: fn(),
    onNewTicket: fn(),
    workspaces: linearWorkspaces,
    onOpenWorkspace: fn(),
    onCreateClone: fn(),
    onOpenInLinear: fn(),
    onCopy: fn(async () => true),
    onMove: fn(),
    onMoveToBacklog: fn(),
    onCancel: fn(),
  },
} satisfies Meta<typeof TicketsView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The open tickets no clone has, in the operator's order. ⋯ opens a row's actions. */
export const Default: Story = {};

/** One ticket open in the page, with its panel. */
export const OpenTicket: Story = {
  args: {
    open: ticketDetailed,
    detail: (
      <TicketPanel
        ticket={ticketDetailed}
        onCopyBranchName={async () => true}
        onCopyTicketLink={async () => true}
        onCreateClone={fn()}
        onTitleChange={fn()}
        onStateChange={fn()}
        stateOptions={teamStates}
        labelOptions={teamLabels}
        onAddLabel={fn()}
        onRemoveLabel={fn()}
      />
    ),
  },
};

/** Linear has not answered yet. */
export const Loading: Story = { args: { tickets: [], loading: true } };

/** Every open ticket has a clone. */
export const Empty: Story = { args: { tickets: [] } };

/** Linear's last answer failed; the list keeps what it had. */
export const LinearError: Story = { args: { error: "Linear answered 401: the API key is not valid" } };
