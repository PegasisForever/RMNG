import { type RouteConfig, route } from "@react-router/dev/routes";

// SPA mode: `/app` (the app; `/` is its shell, `public/shell.html`, which the server sends) and
// `/screen-area` (a test page of the screen's areas, from Settings) are the client routes.
// Everything else — `/events` (SSE), `/api/*`, `/uploads/*`, the MCP ports — is served by the
// Rust control-server same-origin; the client `fetch`es / `EventSource`s / opens WebSockets to
// them directly.
export default [route("app", "routes/app.tsx"), route("screen-area", "routes/screenArea.tsx")] satisfies RouteConfig;
