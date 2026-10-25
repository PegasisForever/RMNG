import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { KeyboardView } from "./KeyboardView";
import { SHIFTS } from "./keyboard";
import type { ModState } from "./modifiers";

/** The keyboard area on the unfolded iPhone Duo: 669 × 412, a 34 px home bar. The container
 *  tells taps from pointer gestures, so here a key only shows that it is touched. */
function Frame(args: { height: number; fn: boolean; mods: [number, ModState][]; caps: boolean }) {
  return (
    <KeyboardView
      width={669}
      height={args.height}
      bottom={34}
      fn={args.fn}
      mods={new Map(args.mods)}
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
  args: { height: 412, fn: false, mods: [], caps: false },
} satisfies Meta<typeof Frame>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

/** Shift tapped: the keys show their Shift symbols and upper case letters. */
export const ShiftTapped: Story = { args: { mods: [[SHIFTS[0], "latched"]] } };

/** Shift locked by a double tap: its icon is the Caps Lock arrow. */
export const ShiftLocked: Story = { args: { mods: [[SHIFTS[0], "locked"]] } };

/** Ctrl held by a finger, Alt tapped. */
export const CtrlAltDown: Story = { args: { mods: [[29, "held"], [56, "latched"]] } };

/** Caps Lock on: Caps is lit and the letters are upper case. */
export const CapsOn: Story = { args: { caps: true } };

/** Fn: F1–F12 on the number row, Home/PgUp/PgDn/End on the arrows. */
export const FnOn: Story = { args: { fn: true } };

/** A shorter area: the keys get shorter, the mouse buttons keep what is left. */
export const Short: Story = { args: { height: 330 } };
