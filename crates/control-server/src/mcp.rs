//! Port 3 — the per-clone MCP over stateless JSON-RPC (`initialize`/`ping`/`tools/list`/
//! `tools/call`), the same simple transport the legacy `/mcp` used (curl-testable; no
//! rmcp/SSE session machinery).
//!
//! The in-clone agent connects and self-identifies with its clone id in the
//! `x-rmng-clone` header (clone IPs are dynamic Docker IPAM now, so there is nothing to
//! reverse-map a source IP against). The only tool is `set_state`: the desktop/window
//! tools live on each clone's daemon MCP (`:9004`) and the fleet-facing proxy is the
//! control-server's `POST /api/hosts/:id/mcp` web route (`web::proxy_to_daemon`).

use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use serde_json::{Value, json};

use crate::app::App;

#[derive(Clone)]
struct McpState {
    app: App,
}

pub async fn serve(app: App, port: u16) -> anyhow::Result<()> {
    let state = McpState { app };
    let router = Router::new().route("/", post(rpc)).with_state(state);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("port 3 (per-clone MCP, header-routed) on http://{addr}");
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

/// The per-clone tool set: just `set_state` (the clone is derived from the
/// `x-rmng-clone` header).
fn tools() -> Value {
    let set_state_props =
        json!({ "report": { "type": "string", "enum": ["working", "idle"] }, "note": { "type": "string" } });
    Value::Array(vec![tool(
        "set_state",
        "Report the agent's desktop verdict + a note",
        set_state_props,
        json!([]),
    )])
}

async fn rpc(State(st): State<McpState>, headers: HeaderMap, Json(req): Json<Value>) -> Json<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));
    // Per-clone self-identification: the agent-wrapper sends its clone id (hostname)
    // on every request.
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
        "tools/list" => Ok(json!({ "tools": tools() })),
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
    match name {
        "set_state" => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names() -> Vec<String> {
        tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn per_clone_scope_serves_only_set_state() {
        assert_eq!(tool_names(), vec!["set_state"]);
    }
}
