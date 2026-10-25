import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { KeyboardView } from "./KeyboardView";
import { SHIFTS } from "./keyboard";
import type { DictationState } from "./dictation";
import type { NgTab } from "./ToolRow";
import type { ModState } from "./modifiers";

/** The keyboard area on the unfolded iPhone Duo: 669 × 412, a 34 px home bar. The container
 *  tells taps from pointer gestures, so here a key only shows that it is touched. */
function Frame(args: {
  height: number;
  fn: boolean;
  mods: [number, ModState][];
  caps: boolean;
  tab: NgTab | null;
  pad: boolean;
  dictation: DictationState;
}) {
  const [tab, setTab] = useState(args.tab);
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
      usage={{ cpu: "23%", mem: "41.2GB", disk: "812.4GB" }}
      tab={tab}
      onTab={(t) => setTab((open) => (open === t ? null : t))}
      pad={args.pad}
      dictation={args.dictation}
    />
  );
}

const meta = {
  title: "ng/KeyboardView",
  component: Frame,
  parameters: { layout: "centered" },
  args: { height: 412, fn: false, mods: [], caps: false, tab: null, pad: false, dictation: "off" },
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

/** Fn: F1–F12 on the symbol row; Home, PgUp, PgDn, End and the arrows on the number row. */
export const FnOn: Story = { args: { fn: true } };

/** Dictation listening: its key, between the comma and Fn, is red. */
export const Dictating: Story = { args: { dictation: "listening" } };

/** The keys in use as a trackpad: faded to a trackpad's outline. */
export const Trackpad: Story = { args: { pad: true } };

/** The Usage tab open: the keys and the mouse buttons make way. The page that takes their
 *  place is the container's; the pages have their own stories (ng/Pages). */
export const UsageTab: Story = { args: { tab: "usage" } };

/** A shorter area: the keys get shorter, the mouse buttons keep what is left. */
export const Short: Story = { args: { height: 330 } };
