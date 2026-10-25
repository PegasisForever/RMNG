import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { KeyboardView } from "./KeyboardView";
import { SHIFTS } from "./keyboard";

/** The keyboard area on the unfolded iPhone Duo: 669 × 410 with a 34 px home bar. */
function Frame(args: { fn: boolean; latched: number[]; caps: boolean }) {
  const [latched, setLatched] = useState<ReadonlySet<number>>(new Set(args.latched));
  const [fnOn, setFn] = useState(args.fn);
  return (
    <div style={{ width: 669, height: 410 }}>
      <KeyboardView
        fn={fnOn}
        latched={latched}
        caps={args.caps}
        bottom={34}
        onKey={fn()}
        onModifier={(code) => {
          const next = new Set(latched);
          if (!next.delete(code)) next.add(code);
          setLatched(next);
        }}
        onButton={fn()}
        onScroll={fn()}
        onFn={() => setFn((f) => !f)}
        onDot={fn()}
      />
    </div>
  );
}

const meta = {
  title: "ng/KeyboardView",
  component: Frame,
  parameters: { layout: "centered" },
  args: { fn: false, latched: [], caps: false },
} satisfies Meta<typeof Frame>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

/** Shift held down: the keys show their Shift symbols and upper case letters. */
export const ShiftHeld: Story = { args: { latched: [SHIFTS[0]] } };

/** Ctrl and Alt held down. */
export const CtrlAltHeld: Story = { args: { latched: [29, 56] } };

/** Fn: F1–F12 on the top row, Home/PgUp/PgDn/End on the arrows. */
export const FnOn: Story = { args: { fn: true } };
