// The space of a /ng page for the page stories: a phone's pages panel (377 × 640 CSS px), on
// glass over a desktop picture blurred and darkened by the pages' backdrop, as over the clone's
// screen. `parameters.surface: "window"` puts
// it on the solid ground of the pages' own window instead; `parameters.frameWidth` makes it
// another width (the wide pages' rail). The look is the toolbar's Theme.
import type { Decorator } from "@storybook/react-vite";

import { FakeDesktop } from "../../__fixtures__/desktop";
import { PAGES_BACKDROP, PAGES_GLASS } from "../ui";

export const PAGE_SIZE = { width: 377, height: 640 };

export const inPageFrame: Decorator = (Story, ctx) => {
  const size = { ...PAGE_SIZE, width: (ctx.parameters.frameWidth as number | undefined) ?? PAGE_SIZE.width };
  return ctx.parameters.surface === "window" ? (
    <div className="relative overflow-hidden bg-neutral-100 dark:bg-neutral-950" style={size}>
      <Story />
    </div>
  ) : (
    <div className="relative overflow-hidden p-2" style={{ width: size.width + 16, height: size.height + 16 }}>
      <FakeDesktop />
      <div className={`absolute inset-0 ${PAGES_BACKDROP}`} />
      <div className={`relative h-full overflow-hidden ${PAGES_GLASS}`}>
        <Story />
      </div>
    </div>
  );
};
