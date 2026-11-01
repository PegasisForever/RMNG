// The space of a control panel tab for the tab stories: a phone's control panel (377 × 640 CSS px), on
// glass over a desktop picture darkened by the control panel's backdrop, as over the clone's
// screen. `parameters.surface: "window"` puts
// it on the solid ground of the control panel's own window instead; `parameters.frameWidth` makes it
// another width (the wide control panel's rail). The look is the toolbar's Theme.
import type { Decorator } from "@storybook/react-vite";

import { FakeDesktop } from "../../__fixtures__/desktop";
import { COLUMN_GLASS, CONTROL_PANEL_BACKDROP, CONTROL_PANEL_DIM, CONTROL_PANEL_GLASS } from "../ui";

const TAB_SIZE = { width: 377, height: 640 };

export const inTabFrame: Decorator = (Story, ctx) => {
  const size = { ...TAB_SIZE, width: (ctx.parameters.frameWidth as number | undefined) ?? TAB_SIZE.width };
  return ctx.parameters.surface === "window" ? (
    <div className="relative overflow-hidden bg-neutral-100 dark:bg-neutral-950" style={size}>
      <Story />
    </div>
  ) : (
    <div className="relative overflow-hidden p-2" style={{ width: size.width + 16, height: size.height + 16 }}>
      <FakeDesktop />
      {/* Another width is a wide control panel's column, flat on the columns' shared blur; the
          phone's panel blurs for itself, on a backdrop that only darkens */}
      <div className={`absolute inset-0 ${ctx.parameters.frameWidth ? CONTROL_PANEL_BACKDROP : CONTROL_PANEL_DIM}`} />
      <div className={`relative h-full ${ctx.parameters.frameWidth ? COLUMN_GLASS : `overflow-hidden ${CONTROL_PANEL_GLASS}`}`}>
        <Story />
      </div>
    </div>
  );
};
