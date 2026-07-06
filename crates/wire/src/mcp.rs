//! Desktop-automation tool DTOs + the `rmng desktop` proxy request.
//!
//! [`McpCallRequest`] is what the `rmng` CLI POSTs to `POST /api/hosts/:id/mcp`; the
//! control-server proxies it to the target clone's daemon MCP (`:9004`), which owns the
//! per-tool schema. The remaining DTOs describe those daemon tool inputs (a [`Target`]
//! selects the clone).

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

/// How a tool call selects its clone. Port 3 omits it (IP-routed); port 4 sets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Target {
    /// Clone id (port 4 only). When absent, the server uses the caller's source IP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone: Option<String>,
}

/// Mouse button for click tools (maps to evdev `BTN_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ScreenshotArgs {
    #[serde(flatten)]
    pub target: Target,
    /// Which monitor (default 0).
    #[serde(default)]
    pub monitor: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClickArgs {
    #[serde(flatten)]
    pub target: Target,
    #[serde(default)]
    pub monitor: u32,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub button: MouseButton,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MoveArgs {
    #[serde(flatten)]
    pub target: Target,
    #[serde(default)]
    pub monitor: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TypeArgs {
    #[serde(flatten)]
    pub target: Target,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KeyArgs {
    #[serde(flatten)]
    pub target: Target,
    /// X11 keysym (or a `+`-joined chord, resolved server-side).
    pub keysym: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ScrollArgs {
    #[serde(flatten)]
    pub target: Target,
    #[serde(default)]
    pub monitor: u32,
    #[serde(default)]
    pub dx: i32,
    #[serde(default)]
    pub dy: i32,
}

/// The agent's `set_state` report (the old `/mcp` tool).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SetStateArgs {
    #[serde(flatten)]
    pub target: Target,
    /// "working" | "idle".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A screenshot result: a PNG, base64-encoded for the JSON-RPC payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotResult {
    pub width: u32,
    pub height: u32,
    #[serde(with = "crate::socket::serde_bytes_b64")]
    pub png: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_flattens_target() {
        // port-4 form: clone selector flattened alongside the args.
        let json = r#"{"clone":"c-1","x":10,"y":20,"button":"right"}"#;
        let a: ClickArgs = serde_json::from_str(json).unwrap();
        assert_eq!(a.target.clone.as_deref(), Some("c-1"));
        assert_eq!(a.button, MouseButton::Right);
        // port-3 form: no clone.
        let a2: ClickArgs = serde_json::from_str(r#"{"x":1,"y":2}"#).unwrap();
        assert!(a2.target.clone.is_none());
        assert_eq!(a2.button, MouseButton::Left);
    }
}
