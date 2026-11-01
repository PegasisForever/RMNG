import { type RouteConfig, index, route } from "@react-router/dev/routes";

// SPA mode: the dashboard, the browser viewer (one window per monitor), `/ng/app` (the /ng
// app; `/ng` itself is its shell, `public/ng-shell.html`, which the server sends) and
// `/ng/screen-area` (a test page of the screen's areas, from /ng's Settings) are the client
// routes. Everything else — `/events` (SSE), `/api/*`, `/uploads/*`, the MCP
// ports — is served by the Rust control-server same-origin; the client `fetch`es /
// `EventSource`s / opens WebSockets to them directly.
export default [
  index("routes/_index.tsx"),
  route("viewer", "routes/viewer.tsx"),
  route("ng/app", "routes/ng.tsx"),
  route("ng/screen-area", "routes/ngScreenArea.tsx"),
] satisfies RouteConfig;
