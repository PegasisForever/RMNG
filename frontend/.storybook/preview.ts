import type { Preview } from "@storybook/react-vite";

// The app's global stylesheet — pulls in Tailwind + the base font/colors so stories
// render with the same look as the real dashboard.
import "../app/app.css";

const preview: Preview = {
  parameters: {
    layout: "fullscreen",
    controls: { matchers: { color: /(background|color)$/i, date: /Date$/i } },
  },
  globalTypes: {
    // Theme toolbar. The real app is auto/device-driven (dark: keys off
    // prefers-color-scheme); this toggle just adds a `.dark` class + sets color-scheme
    // on the story so dark mode is previewable here regardless of the dev's OS.
    theme: {
      description: "Preview theme (the app itself auto-follows the OS)",
      defaultValue: "light",
      toolbar: {
        title: "Theme",
        icon: "circlehollow",
        items: [
          { value: "light", title: "Light", icon: "sun" },
          { value: "dark", title: "Dark", icon: "moon" },
        ],
        dynamicTitle: true,
      },
    },
    // Locale toolbar. Every date and time a View renders comes from a `locale` prop, so
    // flipping this re-renders the whole preview under a different one: en-GB gives
    // "Tue 15:00" and "24 Jul", en-US gives "Tue 3:00 PM" and "Jul 24". The default matches
    // the chat fixtures, which are written for a 24-hour clock.
    locale: {
      description: "Locale the Views format dates and times with",
      defaultValue: "en-GB",
      toolbar: {
        title: "Locale",
        icon: "globe",
        items: [
          { value: "en-GB", title: "en-GB (24h, day first)" },
          { value: "en-US", title: "en-US (12h, month first)" },
        ],
        dynamicTitle: true,
      },
    },
  },
  decorators: [
    (Story, ctx) => {
      const dark = ctx.globals.theme === "dark";
      if (typeof document !== "undefined") {
        const root = document.documentElement;
        root.classList.toggle("dark", dark);
        root.style.colorScheme = dark ? "dark" : "light";
        document.body.style.background = dark ? "#0b0f1a" : "#f4f5f7";
      }
      return Story();
    },
    // Locale is the fourth seam, and a prop rather than a context read — so it is injected by
    // overriding the arg rather than by wrapping the story in a provider. The toolbar is the
    // one place a reviewer flips every story at once.
    //
    // Only stories that already declare `locale` are touched. Handing the arg to a component
    // that has no such prop would put a stray control on every story in the preview, and the
    // components that take one keep it REQUIRED: this decorator supplies the value, it does
    // not excuse the prop.
    //
    // `args` REPLACES the story's args rather than merging into them, so the spread is not
    // decoration: without it every other prop arrives undefined and the story fails to mount.
    (Story, ctx) =>
      "locale" in ctx.initialArgs
        ? Story({ args: { ...ctx.args, locale: ctx.globals.locale } })
        : Story(),
  ],
};

export default preview;
