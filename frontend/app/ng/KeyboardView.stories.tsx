import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { KeyboardView, type PadMode } from "./KeyboardView";
import { SHIFTS } from "./keyboard";

/** The keyboard area on the unfolded iPhone Duo: 669 × 444, a 34 px home bar. */
function Frame(args: { height: number; fn: boolean; latched: number[]; caps: boolean; mode: PadMode }) {
  const [latched, setLatched] = useState<ReadonlySet<number>>(new Set(args.latched));
  const [fnOn, setFn] = useState(args.fn);
  const [mode, setMode] = useState(args.mode);
  return (
    <KeyboardView
      width={669}
      height={args.height}
      bottom={34}
      fn={fnOn}
      latched={latched}
      caps={args.caps}
      mode={mode}
      onKey={fn()}
      onModifier={(code) => {
        const next = new Set(latched);
        if (!next.delete(code)) next.add(code);
        setLatched(next);
      }}
      onFn={() => setFn((f) => !f)}
      onTrackpadButton={(down) => down && setMode((m) => (m === "keys" ? "trackpad" : "keys"))}
      onPad={fn()}
    />
  );
}

const meta = {
  title: "ng/KeyboardView",
  component: Frame,
  parameters: { layout: "centered" },
  args: { height: 444, fn: false, latched: [], caps: false, mode: "keys" },
} satisfies Meta<typeof Frame>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

/** Shift held down: the keys show their Shift symbols and upper case letters. */
export const ShiftHeld: Story = { args: { latched: [SHIFTS[0]] } };

/** Fn: F1–F12 on the top row, Home/PgUp/PgDn/End on the arrows. */
export const FnOn: Story = { args: { fn: true } };

/** Trackpad mode: the trackpad takes all the space under the tool row. */
export const Trackpad: Story = { args: { mode: "trackpad" } };

/** A shorter area: the keys get smaller and keep their shape. */
export const Short: Story = { args: { height: 260 } };
