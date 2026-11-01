import type { Config } from "@react-router/dev/config";

export default {
  // No server-side rendering: the build is a static single-page app that the
  // control-server serves. The app gets its state over SSE.
  ssr: false,
} satisfies Config;
