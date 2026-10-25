import { type RouteConfig, index, route } from "@react-router/dev/routes";

// SPA mode: the dashboard, the browser viewer (one window per monitor) and `/ng` (the
// installed phone app, see `public/manifest.webmanifest`) are the client routes. Everything else — `/events` (SSE), `/api/*`, `/uploads/*`, the MCP
// ports — is served by the Rust control-server same-origin; the client `fetch`es /
// `EventSource`s / opens WebSockets to them directly.
export default [
  index("routes/_index.tsx"),
  route("viewer", "routes/viewer.tsx"),
  route("ng", "routes/ng.tsx"),
] satisfies RouteConfig;
