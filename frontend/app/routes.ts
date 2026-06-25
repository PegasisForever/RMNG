import { type RouteConfig, index } from "@react-router/dev/routes";

// SPA mode: only the dashboard is a client route. Everything else — `/events`
// (SSE), `/api/*`, `/uploads/*`, the MCP ports — is served by the Rust
// control-server same-origin; the client `fetch`es / `EventSource`s them directly.
export default [index("routes/_index.tsx")] satisfies RouteConfig;
