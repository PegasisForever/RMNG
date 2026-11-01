import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { LayoutEditorView } from "./LayoutEditorView";
import { DEFAULT_DESKTOP_LAYOUT, type BrowserLayout } from "~/lib/clientLayout";
import { FakeDesktop } from "~/ng/__fixtures__/screen";

/** The dialog on its own draft: drags, size edits, add and remove really change it. `windows`:
 *  a desktop browser's form (it opens a window per monitor), else a phone's. */
function Frame(args: { layout: BrowserLayout; note: string | null; matching: number | null; glass?: boolean; windows: boolean }) {
  const [layout, setLayout] = useState(args.layout);
  return (
    <LayoutEditorView
      layout={layout}
      onChange={setLayout}
      note={args.note}
      matching={args.matching}
      onMatchWindow={fn()}
      onOpenWindow={args.windows ? fn() : undefined}
      onClose={fn()}
      glass={args.glass}
    />
  );
}

const meta = {
  title: "Dashboard/LayoutEditorView",
  component: Frame,
  args: { layout: DEFAULT_DESKTOP_LAYOUT, note: null, matching: null, windows: true },
} satisfies Meta<typeof Frame>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A browser with no layout of its own yet: the default two 1440p monitors, the primary one on
 *  the right. */
export const Default: Story = {};

/** Three monitors: a 4K one in the middle (primary), a portrait one on its left, a laptop
 *  screen under it. */
export const Three: Story = {
  args: {
    layout: {
      ...DEFAULT_DESKTOP_LAYOUT,
      monitors: [
        { width: 3840, height: 2160, x: 1440, y: 0, primary: true },
        { width: 1440, height: 2560, x: 0, y: 0, primary: false },
        { width: 1920, height: 1200, x: 2400, y: 2160, primary: false },
      ],
      scales: [1.5, 1, 1.25],
    },
  },
};

/** A phone's form: its one monitor, its own screen; no windows to open, add or remove. */
export const Phone: Story = {
  args: {
    windows: false,
    layout: { ...DEFAULT_DESKTOP_LAYOUT, monitors: [{ width: 590, height: 1278, x: 0, y: 0, primary: true }], scales: [2] },
  },
};

/** A match for a monitor whose viewer window is not open. */
export const NoWindow: Story = {
  args: { note: "No viewer window is open for monitor 1. Open it, size it, then try again." },
};

/** In `/ng`'s glass, over a clone's desktop: light or dark by the Theme in the toolbar. */
export const Glass: Story = {
  args: { glass: true },
  decorators: [
    (Story) => (
      <div className="relative h-screen">
        <FakeDesktop />
        <Story />
      </div>
    ),
  ],
};
