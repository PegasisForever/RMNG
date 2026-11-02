import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { FakeDesktop } from "~/desk/__fixtures__/desktop";
import { FIELD, TEXT_2 } from "~/desk/control/ui";

import { PrioritySelect } from "./PrioritySelect";

/** The field styling of the dialog this sits in, so the trigger is judged at the size it
 *  ships at rather than as a bare button. */
const field = `mt-1 w-full ${FIELD} px-3 py-2 text-sm font-normal`;

const meta = {
  title: "Tickets/Components/PrioritySelect",
  component: PrioritySelect,
  parameters: { layout: "fullscreen" },
  // Over a clone's screen, where the lists open: the list is glass, the field flat on the page
  decorators: [
    (Story) => (
      <div className="relative flex h-screen w-screen items-start justify-center overflow-hidden bg-black pt-24">
        <FakeDesktop />
        <div className="relative">
          <Story />
        </div>
      </div>
    ),
  ],
  args: { value: 0, onChange: fn(), className: field },
  render: (args) => (
    <div className={`w-56 text-xs font-medium ${TEXT_2}`}>
      Priority
      <PrioritySelect {...args} />
    </div>
  ),
} satisfies Meta<typeof PrioritySelect>;

export default meta;
type Story = StoryObj<typeof meta>;

/** How the dialog opens: nothing ranked yet, on the glyph Linear draws for an unranked issue
 *  (three bars, none filled). */
export const Default: Story = {};

/** Urgent, the one level with a glyph of its own rather than a place on the bars. */
export const Urgent: Story = { args: { value: 1 } };

/** Low: one bar filled, the other two faint. Every level is the same shape at the same size,
 *  so the row a click lands on is read by fill alone. */
export const Low: Story = { args: { value: 4 } };

/** A create is in flight, so the field takes no clicks. */
export const Disabled: Story = { args: { value: 2, disabled: true } };

/** Wired to local state: the menu really opens, the arrows really move, and the pick sticks.
 *  Click the field, or focus it and press the down arrow. */
export const Interactive: Story = {
  render: function Render(args) {
    const [value, setValue] = useState(args.value);
    return (
      <div className={`w-56 text-xs font-medium ${TEXT_2}`}>
        Priority
        <PrioritySelect
          {...args}
          value={value}
          onChange={(level) => {
            setValue(level);
            args.onChange(level);
          }}
        />
      </div>
    );
  },
};
