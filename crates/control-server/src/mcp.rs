//! Ports 3 + 4 — MCP over stateless JSON-RPC (`initialize`/`ping`/`tools/list`/
//! `tools/call`), the same simple transport the legacy `/mcp` used (curl-testable;
//! no rmcp/SSE session machinery).
//!
//!   - **Port 3 (per-clone)**: the in-clone agent connects; the caller self-identifies
//!     with its clone id in the `x-rmng-clone` header (clone IPs are dynamic Docker
//!     IPAM now, so there is nothing to reverse-map a source IP against). Tools:
//!     `set_state`.
//!   - **Port 4 (global)**: the desktop/computer-use tools, proxied to the target
//!     clone's daemon with an explicit `clone` argument. Fleet management (hosts,
//!     clone/delete, images, accounts) lives in the `rmng` CLI over the port-2 web
//!     API — MCP is for tools whose results belong in model context (screenshots),
//!     not for orchestration.

use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::post,
};
use serde_json::{Value, json};

use crate::app::App;

#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    /// Port 3: the clone is the caller (self-identified via the `x-rmng-clone` header).
    PerClone,
    /// Port 4: the proxied desktop tools; each takes an explicit `clone` argument.
    Global,
}

#[derive(Clone)]
struct McpState {
    app: App,
    scope: Scope,
}

pub async fn serve(app: App, port: u16, scope: Scope) -> anyhow::Result<()> {
    let state = McpState { app, scope };
    let router = Router::new().route("/", post(rpc)).with_state(state);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let label = match scope {
        Scope::PerClone => "port 3 (per-clone MCP, header-routed)",
        Scope::Global => "port 4 (global MCP)",
    };
    tracing::info!("{label} on http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

fn tool(name: &str, desc: &str, props: Value, required: Value) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": { "type": "object", "properties": props, "required": required },
    })
}

/// Desktop + window tools the clone-daemon MCP serves (by name). The fleet MCP proxies
/// these to the target clone; the per-clone control-server MCP does NOT serve them (the
/// in-clone agent calls its clone-daemon directly). Keep in sync with
/// `clone-daemon/src/{mcp,windows}.rs`.
fn is_daemon_tool(name: &str) -> bool {
    matches!(
        name,
        "screenshot"
            | "list_monitors"
            | "mouse_move"
            | "left_click"
            | "right_click"
            | "middle_click"
            | "left_double_click"
            | "scroll"
            | "key"
            | "type"
            | "list_windows"
            | "list_apps"
            | "launch_app"
            | "move_window"
    )
}

/// A proxied desktop/window tool def: always takes `clone` (required) plus `extra` props.
fn dtool(name: &str, desc: &str, mut props: Value, extra_required: &[&str]) -> Value {
    merge(&mut props, &json!({ "clone": { "type": "string", "description": "target clone id" } }));
    let mut req = vec![json!("clone")];
    req.extend(extra_required.iter().map(|s| json!(s)));
    tool(name, desc, props, Value::Array(req))
}

fn tools_for(scope: Scope) -> Value {
    let mut tools = vec![];
    // set_state — per-clone only (the clone is derived from the x-rmng-clone header).
    if scope == Scope::PerClone {
        let set_state_props =
            json!({ "report": { "type": "string", "enum": ["working", "idle"] }, "note": { "type": "string" } });
        tools.push(tool("set_state", "Report the agent's desktop verdict + a note", set_state_props, json!([])));
    }

    if scope == Scope::Global {
        // Desktop + window tools — proxied to the target clone-daemon's MCP.
        let xy = || json!({ "x": { "type": "number" }, "y": { "type": "number" }, "monitor": { "type": "integer" } });
        tools.push(dtool("screenshot", "Screenshot a monitor as PNG", json!({ "monitor": { "type": "integer" } }), &[]));
        tools.push(dtool("list_monitors", "List the clone's monitors", json!({}), &[]));
        tools.push(dtool("mouse_move", "Move the pointer to (x,y) with a glide", xy(), &["x", "y"]));
        tools.push(dtool("left_click", "Left-click (optionally move to x,y first)", xy(), &[]));
        tools.push(dtool("right_click", "Right-click (optionally move to x,y first)", xy(), &[]));
        tools.push(dtool("middle_click", "Middle-click (optionally move to x,y first)", xy(), &[]));
        tools.push(dtool("left_double_click", "Double left-click (optionally move to x,y first)", xy(), &[]));
        tools.push(dtool(
            "scroll",
            "Scroll vertically by `amount` notches (positive = down); optional x,y to move first",
            json!({ "amount": { "type": "integer" }, "x": { "type": "number" }, "y": { "type": "number" }, "monitor": { "type": "integer" } }),
            &["amount"],
        ));
        tools.push(dtool("key", "Press a key combo, e.g. \"ctrl+c\", \"Return\", \"alt+Tab\"", json!({ "keys": { "type": "string" } }), &["keys"]));
        tools.push(dtool("type", "Type a unicode string", json!({ "text": { "type": "string" } }), &["text"]));
        tools.push(dtool("list_windows", "List open windows", json!({}), &[]));
        tools.push(dtool("list_apps", "List installed launcher apps", json!({}), &[]));
        tools.push(dtool("launch_app", "Launch an app by desktop-file id", json!({ "id": { "type": "string" } }), &["id"]));
        tools.push(dtool(
            "move_window",
            "Tile a window: mode \"maximize\" (default) or \"center-half\", optionally onto a monitor index",
            json!({ "id": { "type": "integer" }, "monitor": { "type": "integer" }, "mode": { "type": "string", "enum": ["maximize", "center-half"] } }),
            &["id"],
        ));
    }
    Value::Array(tools)
}

fn merge(into: &mut Value, extra: &Value) {
    if let (Value::Object(a), Value::Object(b)) = (into, extra) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
}

async fn rpc(State(st): State<McpState>, headers: HeaderMap, Json(req): Json<Value>) -> Json<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));
    // Per-clone self-identification: the agent-wrapper sends its clone id (hostname)
    // on every request. Absent on the global port (and on stale in-clone builds).
    let caller = headers
        .get("x-rmng-clone")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let result: Result<Value, String> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "rmng-control-server", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_for(st.scope) })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            call_tool(&st, caller.as_deref(), name, args)
                .await
                .map(|content| json!({ "content": content }))
        }
        other => Err(format!("unknown method '{other}'")),
    };

    match result {
        Ok(v) => Json(json!({ "jsonrpc": "2.0", "id": id, "result": v })),
        Err(e) => Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": e } })),
    }
}

/// Resolve which clone a per-clone call targets: the caller's self-reported id
/// (`x-rmng-clone` header), validated against the host list.
fn caller_clone(st: &McpState, caller: Option<&str>) -> Option<String> {
    let id = caller?;
    st.app.store.get().hosts.into_iter().find(|h| h.id == id).map(|h| h.id)
}

/// A single text content item (MCP `content` array element).
fn text(s: impl Into<String>) -> Value {
    json!([{ "type": "text", "text": s.into() }])
}

async fn call_tool(st: &McpState, caller: Option<&str>, name: &str, args: Value) -> Result<Value, String> {
    let app = &st.app;
    // Desktop + window tools → proxy to the target clone-daemon's MCP. Global only; the
    // in-clone agent calls its own clone-daemon directly (the per-clone MCP is set_state).
    if is_daemon_tool(name) {
        if st.scope != Scope::Global {
            return Err(
                "desktop tools are served by the clone-daemon MCP directly; this per-clone MCP only exposes set_state".into(),
            );
        }
        let clone = args.get("clone").and_then(Value::as_str).ok_or("clone required")?;
        let host = app.store.get().hosts.into_iter().find(|h| h.id == clone).ok_or("unknown clone")?;
        return proxy_to_daemon(app, &host, name, &args).await;
    }
    match name {
        "set_state" if st.scope == Scope::PerClone => {
            let clone = caller_clone(st, caller).ok_or("could not resolve target clone")?;
            let report = args.get("report").and_then(Value::as_str).map(str::to_string);
            let note = args.get("note").and_then(Value::as_str).map(str::to_string);
            app.store.mutate(|s| {
                if let Some(h) = s.hosts.iter_mut().find(|h| h.id == clone) {
                    if let Some(r) = &report {
                        h.agent_report = match r.as_str() {
                            "working" => Some(wire::AgentReport::Working),
                            "idle" => Some(wire::AgentReport::Idle),
                            _ => h.agent_report,
                        };
                    }
                    if note.is_some() {
                        h.state_note = note.clone();
                    }
                }
            });
            Ok(text(format!("state updated for {clone}")))
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

/// Proxy a desktop/window `tools/call` to a clone's clone-daemon MCP (dialed by container
/// name via Docker DNS — `App::dial_host`) and return its `result.content`. The full args
/// (incl. `clone`) pass through; the daemon ignores the `clone` key.
async fn proxy_to_daemon(app: &App, host: &wire::Host, name: &str, args: &Value) -> Result<Value, String> {
    let port = app.config().listen.daemon_mcp;
    let url = format!("http://{}:{port}/", app.dial_host(host).await);
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let resp = app
        .http
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("clone-daemon MCP unreachable at {url}: {e}"))?;
    let body: Value = resp.json().await.map_err(|e| format!("decoding clone-daemon MCP reply: {e}"))?;
    if let Some(err) = body.get("error") {
        return Err(format!("clone-daemon MCP error: {err}"));
    }
    body.get("result")
        .and_then(|r| r.get("content"))
        .cloned()
        .ok_or_else(|| "clone-daemon MCP result missing content".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names(scope: Scope) -> Vec<String> {
        tools_for(scope)
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn global_scope_serves_only_proxied_desktop_tools() {
        let names = tool_names(Scope::Global);
        // Every listed tool is a daemon tool, and every daemon tool is listed.
        assert!(names.iter().all(|n| is_daemon_tool(n)), "non-desktop tool leaked: {names:?}");
        assert_eq!(names.len(), 14, "desktop tool count drifted: {names:?}");
        // The retired fleet tools are gone (fleet management moved to the rmng CLI).
        for retired in [
            "set_state", "list_hosts", "select", "clone", "delete",
            "claude_swap", "codex_swap", "send_message", "read_chat",
        ] {
            assert!(!names.iter().any(|n| n == retired), "retired tool still listed: {retired}");
        }
    }

    #[test]
    fn per_clone_scope_serves_only_set_state() {
        assert_eq!(tool_names(Scope::PerClone), vec!["set_state"]);
    }

    #[test]
    fn every_global_tool_requires_the_clone_arg() {
        for t in tools_for(Scope::Global).as_array().unwrap() {
            let req = t["inputSchema"]["required"].as_array().unwrap();
            assert!(
                req.iter().any(|r| r == "clone"),
                "tool {} does not require `clone`",
                t["name"]
            );
        }
    }
}
