import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SplitView } from "./SplitView";
import { clampSplit, defaultSplit } from "./split";

/** The unfolded iPhone Duo, upright: 669 × 951 with an 82 px clock bar and a 34 px home bar. */
const duo = { width: 669, height: 951, top: 82, bottom: 34 };

/** The line moves in the story too, as it does on the phone. */
function Draggable({ split: start }: { split: number }) {
  const [split, setSplit] = useState(() => clampSplit(start, duo));
  return <SplitView {...duo} split={split} onSplit={setSplit} onSplitEnd={fn()} />;
}

const meta = {
  title: "ng/SplitView",
  component: Draggable,
  parameters: { layout: "centered" },
} satisfies Meta<typeof Draggable>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = { args: { split: defaultSplit(duo) } };

/** The line at its highest: the screen area keeps its least height. */
export const SmallScreen: Story = { args: { split: 0 } };

/** The line at its lowest: the keyboard area keeps its least height. */
export const SmallKeyboard: Story = { args: { split: 951 } };
