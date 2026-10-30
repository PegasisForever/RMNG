import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { LayoutEditorView } from "./LayoutEditorView";
import { DEFAULT_DESKTOP_LAYOUT, type DesktopLayout } from "~/lib/clientLayout";

/** The dialog on its own draft: drags, size edits, add and remove really change it. */
function Frame(args: { layout: DesktopLayout; matchNote: string | null; matching: boolean }) {
  const [layout, setLayout] = useState(args.layout);
  return (
    <LayoutEditorView
      layout={layout}
      onChange={setLayout}
      matchNote={args.matchNote}
      matching={args.matching}
      onMatchWindows={fn()}
      onSave={fn()}
      onClose={fn()}
    />
  );
}

const meta = {
  title: "Dashboard/LayoutEditorView",
  component: Frame,
  args: { layout: DEFAULT_DESKTOP_LAYOUT, matchNote: null, matching: false },
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
      pixelScale: 1.5,
    },
  },
};

/** After Match window sizes, with one window not open. */
export const Matched: Story = {
  args: { matchNote: "Matched 1 monitor(s). No window open for monitor 1: kept its size." },
};
