import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { TicketPanel } from "./TicketPanel";
import TicketDescription from "./TicketDescription";
import { linearTickets } from "~/stories/fixtures";

const detailed = linearTickets[0]; // WE-301: description, sub-issues, due date, estimate
const child = linearTickets[1]; // WE-288: a parent, no sub-issues of its own
const bare = linearTickets[3]; // DEV-97: nothing but a title

/** The real editor, on a fixture. Edits go nowhere: the story spies on the save instead of
 *  writing to Linear. Stories run in the browser, so BlockNote needs no mount gate here —
 *  the lazy import in the route is for the server render, which Storybook does not do. */
function Description({ text }: { text?: string }) {
  return <TicketDescription markdown={text ?? ""} onSave={fn()} />;
}

const meta = {
  title: "Board/TicketPanel",
  component: TicketPanel,
  parameters: { layout: "centered" },
  args: {
    ticket: detailed,
    description: <Description text={detailed.description} />,
    onCreateClone: fn(),
    onTitleChange: fn(),
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
  args: { ticket: child, description: <Description text={child.description} /> },
};

/** Nothing but a title. Every unset property is left out rather than drawn as "None", and
 *  the description is an empty editor waiting to be typed into rather than a "none" line:
 *  the body is editable, so its empty state is the same box its full state is. */
export const Bare: Story = {
  args: { ticket: bare, description: <Description /> },
};

/** The description slot left out entirely, which is what a caller that cannot edit passes.
 *  Only then does the panel say there is nothing there. */
export const NoDescriptionSlot: Story = {
  args: { ticket: bare, description: undefined },
};

/** No clone action and no title editing: the title falls back to a plain heading. */
export const ReadOnly: Story = {
  args: { onCreateClone: undefined, onTitleChange: undefined },
};
