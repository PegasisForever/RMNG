// The space of a /ng page for the page stories: a phone's control panel panel (377 × 640 CSS px), on
// glass over a desktop picture blurred and darkened by the control panel's backdrop, as over the clone's
// screen. `parameters.surface: "window"` puts
// it on the solid ground of the control panel's own window instead; `parameters.frameWidth` makes it
// another width (the wide control panel's rail). The look is the toolbar's Theme.
import type { Decorator } from "@storybook/react-vite";

import { FakeDesktop } from "../../__fixtures__/desktop";
import { CONTROL_PANEL_BACKDROP, CONTROL_PANEL_GLASS } from "../ui";

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
      <div className={`absolute inset-0 ${CONTROL_PANEL_BACKDROP}`} />
      <div className={`relative h-full overflow-hidden ${CONTROL_PANEL_GLASS}`}>
        <Story />
      </div>
    </div>
  );
};
