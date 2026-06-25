import { reactRouter } from "@react-router/dev/vite";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import tsconfigPaths from "vite-tsconfig-paths";

export default defineConfig({
  plugins: [tailwindcss(), reactRouter(), tsconfigPaths()],
  // Match the legacy control-server port so existing clients keep working
  // (they just swap ws:// for http://…/events). `host: true` exposes it on the
  // LAN/tailnet during `bun run dev`.
  server: { port: 9000, host: true },
});
