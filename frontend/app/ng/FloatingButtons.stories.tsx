import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FloatingButtons } from "./FloatingButtons";

/** Over a sideways phone's display (951 × 648), in its top-right corner. */
const meta = {
  title: "ng/FloatingButtons",
  component: FloatingButtons,
  decorators: [
    (Story) => (
      <div className="relative bg-neutral-700" style={{ width: 951, height: 648 }}>
        <Story />
      </div>
    ),
  ],
  parameters: { layout: "centered" },
  args: {
    pagesOpen: false,
    keyboardOpen: false,
    fits: false,
    badge: 0,
    onPages: fn(),
    onKeyboard: fn(),
    onFit: fn(),
  },
} satisfies Meta<typeof FloatingButtons>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The clone's monitor is not this display's size: Fit is live. */
export const Default: Story = {};

/** The keyboard is open, and the monitor fits: Fit is dim. */
export const KeyboardOpenFits: Story = { args: { keyboardOpen: true, fits: true } };

/** Two clones stopped working unseen: a count on Pages. */
export const Unread: Story = { args: { badge: 2, pagesOpen: true } };
