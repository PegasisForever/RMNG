//! The `rmng desktop` proxy request.
//!
//! [`McpCallRequest`] is what the `rmng` CLI POSTs to `POST /api/hosts/:id/mcp`; the
//! control-server proxies it to the target clone's daemon MCP (`:9004`), which owns the
//! per-tool schema.

use serde::{Deserialize, Serialize};

/// A desktop-tool call the `rmng` CLI POSTs to `/api/hosts/:id/mcp`. The control-server
/// wraps it into a JSON-RPC `tools/call` and proxies it verbatim to the target clone's
/// daemon MCP (`:9004`), returning the daemon's `content` array. `args` is the tool's
/// raw argument object (opaque here — the daemon owns the per-tool schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallRequest {
    /// Daemon tool name, e.g. `screenshot` / `left_click` / `type`.
    pub tool: String,
    /// The tool's argument object (passed through as JSON-RPC `arguments`).
    #[serde(default)]
    pub args: serde_json::Value,
}
