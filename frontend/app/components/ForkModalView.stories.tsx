import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FakeDesktop } from "~/desk/__fixtures__/desktop";
import { emptyForkDraft } from "~/lib/forkDraft";
import { ForkModalView } from "./ForkModalView";
import { accountsNow, makeClaudeAccounts, makeGroups } from "./__fixtures__/accounts";
import { makeClone, makeCloneWorking } from "./__fixtures__/clones";
import { makeOperation } from "./__fixtures__/operations";

const ticketClone = makeCloneWorking({ id: "pega-we-142a" });
const plainClone = makeClone({ id: "pega-encoder-scratch", managed: true, displayName: "encoder scratch" });

const meta = {
  title: "Clone/Components/ForkModalView",
  component: ForkModalView,
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
    cloneId: ticketClone.id,
    ticket: { id: "WE-142", title: "Normalize sidebar CPU to % of allowance" },
    parentId: ticketClone.id,
    draft: emptyForkDraft(ticketClone),
    onDraftChange: fn(),
    accounts: makeClaudeAccounts(accountsNow),
    groups: makeGroups(),
    valid: true,
    busy: false,
    error: null,
    operation: null,
    onSubmit: fn(),
    onClose: fn(),
  },
} satisfies Meta<typeof ForkModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A clone with a ticket, as the dialog opens from its menu: the fork keeps the ticket, and
 *  is a sub clone of the source. */
export const WithTicket: Story = { args: {} };

/** A clone with no ticket: the fork needs a name before Fork works. */
export const NoTicket: Story = {
  args: {
    cloneId: plainClone.id,
    ticket: null,
    parentId: plainClone.id,
    draft: emptyForkDraft(plainClone),
    valid: false,
  },
};

/** A message typed: the agent starts on it once the fork is up. */
export const WithMessage: Story = {
  args: { draft: { ...emptyForkDraft(ticketClone), message: "Try the other fix: cap the CPU at the quota." } },
};

/** The fork of a sub clone goes under the same parent, as a sub clone is never a parent. */
export const OfSubClone: Story = {
  args: { cloneId: "pega-we-142-helper", parentId: ticketClone.id },
};

/** "More options" open: the accounts and the options of the New clone dialog. */
export const MoreOptions: Story = {
  args: { initialMore: true },
};

/** The fork is running: the form and Fork lock, and the op's progress shows. Close still
 *  closes: the fork goes on, and the control panel says if it fails. */
export const Forking: Story = {
  args: {
    busy: true,
    operation: makeOperation({ target: "pega-we-142b" }),
  },
};

/** The start failed: the form stays, so the operator can try again. */
export const WithError: Story = {
  args: { error: "source 'pega-we-142a' is already in an operation" },
};
