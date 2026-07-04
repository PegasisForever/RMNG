//! Ports 3 + 4 — MCP over stateless JSON-RPC (`initialize`/`ping`/`tools/list`/
//! `tools/call`), the same simple transport the legacy `/mcp` used (curl-testable;
//! no rmcp/SSE session machinery).
//!
//!   - **Port 3 (per-clone)**: the in-clone agent connects; the caller self-identifies
//!     with its clone id in the `x-rmng-clone` header (clone IPs are dynamic Docker
//!     IPAM now, so there is nothing to reverse-map a source IP against). Tools:
//!     `set_state`.
//!   - **Port 4 (global)**: every web-frontend action as a tool, plus the desktop
//!     tools with an explicit `clone` argument. Replaces `control-server-ctl`.

use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::post,
};
use serde_json::{Value, json};

use crate::app::App;
use crate::jobs::{self, CloneSpec};

#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    /// Port 3: the clone is the caller (self-identified via the `x-rmng-clone` header).
    PerClone,
    /// Port 4: global control; tools take an explicit `clone` + web actions.
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
    let clone_arg = json!({ "clone": { "type": "string", "description": "target clone id" } });
    let mut tools = vec![];
    // set_state — both scopes (PerClone derives the clone from the x-rmng-clone header).
    let set_state_props = if scope == Scope::Global {
        json!({ "clone": { "type": "string" }, "report": { "type": "string", "enum": ["working", "idle"] }, "note": { "type": "string" } })
    } else {
        json!({ "report": { "type": "string", "enum": ["working", "idle"] }, "note": { "type": "string" } })
    };
    tools.push(tool("set_state", "Report the agent's desktop verdict + a note", set_state_props, json!([])));

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

        // Web/control actions (handled locally; replace control-server-ctl).
        tools.push(tool("list_hosts", "List all hosts + their state", json!({}), json!([])));
        tools.push(tool("select", "Select the host shown in the viewer", clone_arg.clone(), json!(["clone"])));
        tools.push(tool(
            "clone",
            "Create a clone from a source image (e.g. pegasis0/rmng-template:latest)",
            json!({ "image": { "type": "string", "description": "clone-source image reference" }, "hostname": { "type": "string" } }),
            json!(["image", "hostname"]),
        ));
        tools.push(tool("delete", "Delete a host", clone_arg.clone(), json!(["clone"])));
        tools.push(tool(
            "claude_swap",
            "Hot-swap a clone's Claude account",
            json!({
                "clone": { "type": "string" },
                "account": {
                    "type": "string",
                    "description": "An account email, \"auto\" (server picks best), \"group:<name>\", or \"none\" (remove the clone's token)",
                },
            }),
            json!(["clone", "account"]),
        ));
        tools.push(tool(
            "codex_swap",
            "Hot-swap a clone's Codex account",
            json!({
                "clone": { "type": "string" },
                "account": {
                    "type": "string",
                    "description": "An account email, \"auto\" (server picks best), \"group:<name>\", or \"none\" (remove the clone's token)",
                },
            }),
            json!(["clone", "account"]),
        ));
        tools.push(tool(
            "send_message",
            "Send a chat message to a clone's host agent. Async: the agent works in the background — poll read_chat for its reply. Errors if a turn is already running for that clone.",
            json!({
                "clone": { "type": "string", "description": "target clone id" },
                "text": { "type": "string", "description": "the message to send to the host agent" },
            }),
            json!(["clone", "text"]),
        ));
        tools.push(tool(
            "read_chat",
            "Read a clone's host-agent chat history + live working state: { busy, activity?, messages[] }. `busy` = a turn is running; `activity` = what it's doing right now.",
            clone_arg.clone(),
            json!(["clone"]),
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

/// Resolve which clone a call targets: per-clone scope → the caller's self-reported id
/// (`x-rmng-clone` header), validated against the host list; global → `clone` arg.
fn target_clone(st: &McpState, caller: Option<&str>, args: &Value) -> Option<String> {
    match st.scope {
        Scope::Global => args.get("clone").and_then(Value::as_str).map(str::to_string),
        Scope::PerClone => {
            let id = caller?;
            st.app.store.get().hosts.into_iter().find(|h| h.id == id).map(|h| h.id)
        }
    }
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
        "set_state" => {
            let clone = target_clone(st, caller, &args).ok_or("could not resolve target clone")?;
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
        "list_hosts" => {
            let hosts = app.store.get().hosts;
            serde_json::to_string(&hosts).map(text).map_err(|e| e.to_string())
        }
        "select" => {
            let clone = args.get("clone").and_then(Value::as_str).ok_or("clone required")?.to_string();
            app.store.mutate(|s| s.selected = Some(clone.clone()));
            Ok(text(format!("selected {clone}")))
        }
        "clone" => {
            let image = args.get("image").and_then(Value::as_str).ok_or("image required")?.to_string();
            let hostname = args.get("hostname").and_then(Value::as_str).ok_or("hostname required")?.to_string();
            let op = jobs::start_clone(
                app,
                CloneSpec {
                    source_image: image,
                    new_hostname: hostname,
                    agent_playbook: crate::web::compose_playbook(&app.config(), None),
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
            Ok(text(format!("clone started: op {}", op.id)))
        }
        "delete" => {
            let clone = args.get("clone").and_then(Value::as_str).ok_or("clone required")?;
            let op = jobs::start_delete(app, clone).map_err(|e| e.to_string())?;
            Ok(text(format!("delete started: op {}", op.id)))
        }
        "claude_swap" => {
            let clone = args.get("clone").and_then(Value::as_str).ok_or("clone required")?;
            let account = args.get("account").and_then(Value::as_str).unwrap_or("auto");
            let host = app.store.get().hosts.into_iter().find(|h| h.id == clone).ok_or("unknown clone")?;
            if !host.managed {
                return Err("not a managed clone".into());
            }
            let assignment = crate::claude::resolve_assignment(app, Some(account)).ok_or("no imported Claude accounts")?;
            let selection = crate::claude::normalize_selection(Some(account));
            let (group, email) = match assignment {
                crate::claude::Assignment::None => {
                    crate::claude::clear_clone_token(app, &host.id)
                        .await
                        .map_err(|e| e.to_string())?;
                    app.claude.forget_pushed(&host.id);
                    (None, None)
                }
                crate::claude::Assignment::Group { name, initial } => {
                    crate::claude::push_account_to_clone(app, &host.id, &initial)
                        .await
                        .map_err(|e| e.to_string())?;
                    (Some(name), Some(initial))
                }
                crate::claude::Assignment::Account(a) => {
                    crate::claude::push_account_to_clone(app, &host.id, &a)
                        .await
                        .map_err(|e| e.to_string())?;
                    (None, Some(a))
                }
            };
            let (id, email_set, group_set, sel_set) =
                (host.id.clone(), email.clone(), group.clone(), selection.clone());
            app.store.mutate(|s| {
                if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                    h.claude_account_email = email_set;
                    h.claude_group = group_set;
                    h.claude_selection = Some(sel_set);
                }
            });
            Ok(text(match (&group, &email) {
                (Some(g), Some(e)) => format!("swapped {clone} → {e} (group {g})"),
                (None, Some(e)) => format!("swapped {clone} → {e}"),
                _ => format!("swapped {clone} → none (no token)"),
            }))
        }
        "codex_swap" => {
            let clone = args.get("clone").and_then(Value::as_str).ok_or("clone required")?;
            let account = args.get("account").and_then(Value::as_str).unwrap_or("auto");
            let host = app.store.get().hosts.into_iter().find(|h| h.id == clone).ok_or("unknown clone")?;
            if !host.managed {
                return Err("not a managed clone".into());
            }
            let assignment =
                crate::codex::resolve_assignment(app, Some(account)).ok_or("no imported Codex accounts")?;
            let selection = crate::codex::normalize_selection(Some(account));
            let (group, email) = match assignment {
                crate::codex::Assignment::None => {
                    crate::codex::clear_clone_token(app, &host.id).await.map_err(|e| e.to_string())?;
                    app.codex.forget_pushed(&host.id);
                    (None, None)
                }
                crate::codex::Assignment::Group { name, initial } => {
                    crate::codex::push_account_to_clone(app, &host.id, &initial)
                        .await
                        .map_err(|e| e.to_string())?;
                    (Some(name), Some(initial))
                }
                crate::codex::Assignment::Account(a) => {
                    crate::codex::push_account_to_clone(app, &host.id, &a)
                        .await
                        .map_err(|e| e.to_string())?;
                    (None, Some(a))
                }
            };
            let (id, email_set, group_set, sel_set) =
                (host.id.clone(), email.clone(), group.clone(), selection.clone());
            app.store.mutate(|s| {
                if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                    h.codex_account_email = email_set;
                    h.codex_group = group_set;
                    h.codex_selection = Some(sel_set);
                }
            });
            Ok(text(match (&group, &email) {
                (Some(g), Some(e)) => format!("swapped {clone} codex → {e} (group {g})"),
                (None, Some(e)) => format!("swapped {clone} codex → {e}"),
                _ => format!("swapped {clone} codex → none (no token)"),
            }))
        }
        "send_message" => {
            let clone = args.get("clone").and_then(Value::as_str).ok_or("clone required")?;
            let msg = args.get("text").and_then(Value::as_str).ok_or("text required")?;
            let host = app.store.get().hosts.into_iter().find(|h| h.id == clone).ok_or("unknown clone")?;
            crate::chat::send_chat(app, &host, msg)?;
            Ok(text(format!("message sent to {clone}; the host agent is now working — poll read_chat for its reply")))
        }
        "read_chat" => {
            let clone = args.get("clone").and_then(Value::as_str).ok_or("clone required")?;
            if !app.store.get().hosts.iter().any(|h| h.id == clone) {
                return Err("unknown clone".into());
            }
            Ok(text(crate::chat::snapshot_json(app, clone)))
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
