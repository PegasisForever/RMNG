import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SplitDialog } from "./SplitDialog";

/** The dialog over a black iPhone Duo screen (669 × 951); the slider moves. */
function Frame({ start }: { start: number }) {
  const [value, setValue] = useState(start);
  return (
    <div className="relative bg-black" style={{ width: 669, height: 951 }}>
      <SplitDialog value={value} min={150} max={751} total={951} standard={444} onChange={setValue} onClose={fn()} />
    </div>
  );
}

const meta = {
  title: "ng/SplitDialog",
  component: Frame,
  parameters: { layout: "centered" },
  args: { start: 444 },
} satisfies Meta<typeof Frame>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};
