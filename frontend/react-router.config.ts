import type { Config } from "@react-router/dev/config";

export default {
  // Server-side rendering: the dashboard loader returns the current state on
  // first paint; the client then patches it live over SSE.
  ssr: false,
} satisfies Config;
