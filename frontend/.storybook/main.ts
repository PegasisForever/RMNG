import type { StorybookConfig } from "@storybook/react-vite";
import type { PluginOption } from "vite";

/** Recursively await + flatten Vite's PluginOption tree (entries may be promises
 *  or nested arrays) into a flat list we can filter by plugin name. */
async function flattenPlugins(plugins: PluginOption[]): Promise<{ name?: string }[]> {
  const out: { name?: string }[] = [];
  for (const entry of plugins) {
    const resolved = await entry;
    if (!resolved) continue;
    if (Array.isArray(resolved)) out.push(...(await flattenPlugins(resolved)));
    else out.push(resolved as { name?: string });
  }
  return out;
}

const config: StorybookConfig = {
  stories: ["../app/**/*.stories.@(ts|tsx)"],
  addons: [],
  framework: { name: "@storybook/react-vite", options: {} },
  async viteFinal(cfg) {
    // Storybook auto-merges the project's vite.config.ts — that's how Tailwind 4
    // (@tailwindcss/vite) and the `~/*` path alias (vite-tsconfig-paths) reach the
    // stories. But the React Router dev plugin owns the app's build/SSR pipeline
    // (route manifest, server entry) and breaks Storybook's component-only build,
    // so strip just that one and keep the rest.
    const flat = await flattenPlugins((cfg.plugins ?? []) as PluginOption[]);
    cfg.plugins = flat.filter((p) => !String(p?.name ?? "").startsWith("react-router"));
    return cfg;
  },
};

export default config;
