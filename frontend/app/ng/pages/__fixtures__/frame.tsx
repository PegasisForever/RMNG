// A small space for a /ng page (661 × 334 CSS px), in the
// phone app's dark greys, for the page stories.
import type { Decorator } from "@storybook/react-vite";

export const PAGE_SIZE = { width: 661, height: 334 };

export const inPageFrame: Decorator = (Story) => (
  <div className="ng-theme dark relative overflow-hidden bg-black text-white" style={PAGE_SIZE}>
    <Story />
  </div>
);
