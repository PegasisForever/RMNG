import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FakeDesktop } from "./__fixtures__/screen";
import { KeyboardView, keyboardPanel } from "./KeyboardView";
import { SHIFTS } from "./keyboard";
import type { DictationState } from "./dictation";
import type { ModState } from "./modifiers";

/** The keyboard on a large phone, upright: 669 px wide, 45% of 951 px high, a 34 px
 *  home bar. The container tells taps from pointer gestures, so here a key only shows that it is
 *  touched. It is on its panel: glass over a desktop, or solid when docked. It follows the
 *  toolbar's Theme. */
function Frame(args: {
  width: number;
  height: number;
  bottom: number;
  fn: boolean;
  mods: [number, ModState][];
  caps: boolean;
  pad: boolean;
  dictation: DictationState;
  docked: boolean;
}) {
  return (
    <div className="relative bg-black" style={{ width: args.width, height: args.height + 120 }}>
      {args.docked ? null : <FakeDesktop />}
      <div className={`absolute bottom-0 left-0 ${keyboardPanel(args.docked)}`}>
        <KeyboardView
          width={args.width}
          height={args.height}
          bottom={args.bottom}
          fn={args.fn}
          mods={new Map(args.mods)}
          caps={args.caps}
          onKeyTouch={fn()}
          pad={args.pad}
          dictation={args.dictation}
        />
      </div>
    </div>
  );
}

const meta = {
  title: "Desk/KeyboardView",
  component: Frame,
  parameters: { layout: "centered" },
  args: { width: 669, height: 428, bottom: 34, fn: false, mods: [], caps: false, pad: false, dictation: "off", docked: false },
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

/** Docked under the screen: a solid panel, nothing to see through it. */
export const Docked: Story = { args: { docked: true } };

/** On a sideways phone (951 px wide, 45% of 648 px high): the symbol and number rows short. */
export const Sideways: Story = { args: { width: 951, height: 292, bottom: 21 } };
