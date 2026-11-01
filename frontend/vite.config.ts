import { reactRouter } from "@react-router/dev/vite";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import tsconfigPaths from "vite-tsconfig-paths";

export default defineConfig({
  plugins: [tailwindcss(), reactRouter(), tsconfigPaths()],
  // `bun run dev` serves the pages only: there is no proxy to the control-server, so the
  // API and the clone's screen do not work here. `host: true` exposes it on the LAN/tailnet.
  server: { port: 9000, host: true },
  // The media socket's worker loads its WebAssembly Opus decoder only where it needs one
  // (`app/viewer/opus.ts`), and a lazy import needs ES-module workers. Every worker is started
  // with `type: "module"`.
  worker: { format: "es" },
});
