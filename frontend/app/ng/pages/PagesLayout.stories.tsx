import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { FakeDesktop } from "../__fixtures__/desktop";
import { pagesFor } from "../__fixtures__/screen";
import type { NgTab } from "../ToolRow";
import { PAGES_BACKDROP, PAGES_GLASS } from "./ui";
import { WIDE_MIN } from "./WideLayout";

/** The /ng pages, all of them, as their container lays them out: narrow, the tool row over a
 *  tab's page; wide (`WIDE_MIN`), the dashboard's layout. On glass over the clone's screen, or
 *  on the solid ground of their own window. The look is the toolbar's Theme. */
function Pages({ width, height, surface, tab: first }: { width: number; height: number; surface: "glass" | "window"; tab: NgTab }) {
  const [tab, setTab] = useState(first);
  const glass = surface === "glass";
  const content = pagesFor({ width, glass, tab, onTab: setTab, badges: { clones: 2 }, onPopOut: glass ? fn() : undefined });
  return surface === "window" ? (
    <div className="relative overflow-hidden bg-neutral-100 dark:bg-neutral-950" style={{ width, height }}>
      {content}
    </div>
  ) : (
    // The pages' box over the screen: in from the sides narrow; the display's whole width wide
    <div
      className={`relative overflow-hidden ${width >= WIDE_MIN ? "py-2" : "p-2"}`}
      style={{ width: width >= WIDE_MIN ? width : width + 16, height: height + 16 }}
    >
      <FakeDesktop />
      <div className={`absolute inset-0 ${PAGES_BACKDROP}`} />
      {/* Narrow, one glass panel; wide, each column is glass of its own */}
      <div className={`relative h-full ${width >= WIDE_MIN ? "" : `overflow-hidden ${PAGES_GLASS}`}`}>{content}</div>
    </div>
  );
}

const meta = {
  title: "ng/Pages/Layout",
  component: Pages,
  parameters: { layout: "centered" },
  args: { width: 377, height: 750, surface: "glass", tab: "clones" },
} satisfies Meta<typeof Pages>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A phone upright: the tabs; the server's numbers are on the Usage page. */
export const Phone: Story = {};

/** A phone upright, on the Usage page: the server's numbers at its top. */
export const PhoneUsage: Story = { args: { tab: "usage" } };

/** A phone on its side: the tabs, with the server's numbers in the tool row. */
export const PhoneSideways: Story = { args: { width: 935, height: 340, tab: "tickets" } };

/** A desktop's large screen: the dashboard's layout, on glass. */
export const Wide: Story = { args: { width: 1424, height: 820 } };

/** The pages' own window, wide: the dashboard's layout on a solid ground. */
export const WideWindow: Story = { args: { width: 1440, height: 900, surface: "window" } };

/** Wide, with Settings open in the board's place. */
export const WideSettings: Story = { args: { width: 1424, height: 820, tab: "settings" } };

/** The pages' own window, narrow. */
export const NarrowWindow: Story = { args: { width: 560, height: 900, surface: "window" } };
