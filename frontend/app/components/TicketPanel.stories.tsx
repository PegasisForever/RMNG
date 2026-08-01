import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { TicketPanel } from "./TicketPanel";
import TicketDescription from "./TicketDescription";
// ticketDetailed is WE-301 (description, sub-issues, due date, estimate), ticketSubIssue is
// WE-288 (a parent, no sub-issues of its own), and ticketBare is DEV-97 (nothing but a title).
import { ticketBare, ticketDetailed, ticketSubIssue } from "./__fixtures__/tickets";

/** The real editor, on a fixture. Edits go nowhere: the story spies on the save instead of
 *  writing to Linear. Stories run in the browser, so BlockNote needs no mount gate here —
 *  the lazy import in the route is for the server render, which Storybook does not do. */
function Description({ text }: { text?: string }) {
  return <TicketDescription markdown={text ?? ""} onSave={fn()} />;
}

/** Three referenced issues, three answers. WE-302 has a clone on this board, WE-303 and
 *  WE-280 are in the ticket column, and WE-304 is in neither, so only its row keeps the
 *  arrow and opens Linear. The route resolves the same three ways against the real lists. */
const onBoard: Record<string, string> = {
  "WE-302": "Show the clone for WE-302: mercury",
  "WE-303": "Show WE-303 in this panel",
  "WE-280": "Show WE-280 in this panel",
};
const resolveLink = (id: string) =>
  onBoard[id] ? { title: onBoard[id], open: fn() } : null;

const meta = {
  title: "Board/Components/TicketPanel",
  component: TicketPanel,
  parameters: { layout: "centered" },
  args: {
    ticket: ticketDetailed,
    description: <Description text={ticketDetailed.description} />,
    onCreateClone: fn(),
    onTitleChange: fn(),
    resolveLink,
  },
  /** The panel fills a card in the shell's side column, so the story gives it one of the
   *  same size rather than letting it size to its contents. */
  render: (args) => (
    <div className="h-[42rem] w-[26rem] overflow-hidden rounded-2xl border border-slate-900/10 bg-white shadow-xl dark:border-white/10 dark:bg-slate-900">
      <TicketPanel {...args} />
    </div>
  ),
} satisfies Meta<typeof TicketPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Everything filled in: priority, assignee, estimate, due date, labels, a description, and
 *  three sub-issues with one already done. */
export const Default: Story = {};

/** A sub-issue itself. Its parent gets a row above the properties, which is where Linear
 *  puts it too. */
export const WithParent: Story = {
  args: { ticket: ticketSubIssue, description: <Description text={ticketSubIssue.description} /> },
};

/** Nothing but a title. Every unset property is left out rather than drawn as "None", and
 *  the description is an empty editor waiting to be typed into rather than a "none" line:
 *  the body is editable, so its empty state is the same box its full state is. */
export const Bare: Story = {
  args: { ticket: ticketBare, description: <Description /> },
};

/** The description slot left out entirely, which is what a caller that cannot edit passes.
 *  Only then does the panel say there is nothing there. */
export const NoDescriptionSlot: Story = {
  args: { ticket: ticketBare, description: undefined },
};

/** No clone action and no title editing: the title falls back to a plain heading. Without a
 *  resolver every referenced issue opens Linear, which is what a panel with no board behind
 *  it wants. */
export const ReadOnly: Story = {
  args: { onCreateClone: undefined, onTitleChange: undefined, resolveLink: undefined },
};
