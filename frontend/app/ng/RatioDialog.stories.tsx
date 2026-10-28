import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { RatioDialog } from "./RatioDialog";

/** Over a sideways phone's display (951 × 648 CSS px at 3×), opened by a long press on Fit. */
const meta = {
  title: "ng/RatioDialog",
  component: RatioDialog,
  decorators: [
    (Story) => (
      <div className="ng-theme dark relative bg-neutral-700" style={{ width: 951, height: 648 }}>
        <Story />
      </div>
    ),
  ],
  parameters: { layout: "centered" },
  args: { ratio: 2, width: 951, height: 648, dpr: 3, onChoose: fn(), onClose: fn() },
} satisfies Meta<typeof RatioDialog>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The default: 2 screen px per clone px, as upright. */
export const Default: Story = {};

/** The folded phone (466 × 657), set to 1.5×. */
export const Folded: Story = { args: { width: 466, height: 657, ratio: 1.5 } };
