import type { Preview } from "@storybook/react-vite";

// The app's global stylesheet — pulls in Tailwind + the base font/colors so stories
// render with the same look as the real dashboard.
import "../app/app.css";

const preview: Preview = {
  parameters: {
    layout: "fullscreen",
    controls: { matchers: { color: /(background|color)$/i, date: /Date$/i } },
  },
};

export default preview;
