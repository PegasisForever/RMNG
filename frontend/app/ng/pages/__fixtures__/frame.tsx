// The space of a /ng page for the page stories: a phone's pages panel (377 × 640 CSS px), on
// glass over a desktop picture, as over the clone's screen. `parameters.surface: "window"` puts
// it on the solid ground of the pages' own window instead. The look is the toolbar's Theme.
import type { Decorator } from "@storybook/react-vite";

import { FakeDesktop } from "../../__fixtures__/desktop";
import { PAGES_GLASS } from "../ui";

export const PAGE_SIZE = { width: 377, height: 640 };

export const inPageFrame: Decorator = (Story, ctx) =>
  ctx.parameters.surface === "window" ? (
    <div className="relative overflow-hidden bg-neutral-100 dark:bg-neutral-950" style={PAGE_SIZE}>
      <Story />
    </div>
  ) : (
    <div className="relative overflow-hidden p-2" style={{ width: PAGE_SIZE.width + 16, height: PAGE_SIZE.height + 16 }}>
      <FakeDesktop />
      <div className={`relative h-full overflow-hidden ${PAGES_GLASS}`}>
        <Story />
      </div>
    </div>
  );
