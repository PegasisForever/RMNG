import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { KeyboardView } from "./KeyboardView";
import { SHIFTS } from "./keyboard";

/** The keyboard area on the unfolded iPhone Duo: 669 × 444, a 34 px home bar. The container
 *  tells taps from pointer gestures, so here a key only shows that it is touched. */
function Frame(args: { height: number; fn: boolean; latched: number[]; caps: boolean }) {
  return (
    <KeyboardView
      width={669}
      height={args.height}
      bottom={34}
      fn={args.fn}
      latched={new Set(args.latched)}
      caps={args.caps}
      onKeyTouch={fn()}
      onButton={fn()}
    />
  );
}

const meta = {
  title: "ng/KeyboardView",
  component: Frame,
  parameters: { layout: "centered" },
  args: { height: 444, fn: false, latched: [], caps: false },
} satisfies Meta<typeof Frame>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

/** Shift held down: the keys show their Shift symbols and upper case letters. */
export const ShiftHeld: Story = { args: { latched: [SHIFTS[0]] } };

/** Ctrl and Alt held down. */
export const CtrlAltHeld: Story = { args: { latched: [29, 56] } };

/** Fn: F1–F12 on the number row, Home/PgUp/PgDn/End on the arrows. */
export const FnOn: Story = { args: { fn: true } };

/** A shorter area: the keys get shorter, the mouse buttons keep what is left. */
export const Short: Story = { args: { height: 330 } };
