import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FakeDesktop } from "./__fixtures__/screen";
import { BrowserFullscreenDialog } from "./BrowserFullscreenDialog";

const meta = {
  title: "Desk/BrowserFullscreenDialog",
  component: BrowserFullscreenDialog,
  parameters: { layout: "fullscreen" },
  // Over a clone's desktop, as in the browser's own fullscreen: light or dark by the Theme
  decorators: [
    (Story) => (
      <div className="relative h-screen">
        <FakeDesktop />
        <Story />
      </div>
    ),
  ],
  args: { open: true, leaveKeys: "F11", onExited: fn() },
} satisfies Meta<typeof BrowserFullscreenDialog>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Windows and Linux: F11 leaves the browser's fullscreen. */
export const Default: Story = {};

/** A Mac: Control + Command + F (or the green button) leaves it. */
export const Mac: Story = { args: { leaveKeys: "⌃ Control + ⌘ Command + F" } };
