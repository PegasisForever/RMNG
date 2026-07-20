//! Port 2 — the web API + SSE + static frontend. Phase 1 + the Phase-2 clone/
//! delete surface; the rest (Linear/Claude/chat/config/…) lands as those modules
//! are ported.

use std::convert::Infallible;
use std::path::Path;
use std::time::Duration;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as AxPath, Request, State},
    http::{HeaderName, StatusCode, header},
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{any, get, post, put},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures::stream::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

/// 404 hint when no frontend dir resolves anywhere (image install missing AND no dev
/// build) — the API stays up so this only ever surfaces in a broken/dev environment.
async fn missing_frontend() -> Response {
    (
        StatusCode::NOT_FOUND,
        format!(
            "frontend not installed: expected {}/static (image) or frontend/build/client \
             (dev; run `bun run build` in frontend/)",
            crate::assets::INSTALL_DIR
        ),
    )
        .into_response()
}
use wire::{AppConfigRedacted, ConfigPutResponse, ControlState, Operation};

use crate::app::App;
use crate::config;
use crate::files;
use crate::jobs::{self, CloneSpec, LinearMeta};
use crate::linear;

pub fn router(app: App) -> Router {
    let routes = Router::new()
        .route("/events", get(events))
        .route("/api/state", get(state_get))
        .route("/api/activate", post(activate))
        .route("/api/reorder", post(reorder))
        .route("/api/clone", post(clone))
        .route("/api/layout/activate", post(layout_activate))
        .route("/api/delete", post(delete))
        .route("/api/notes/:id", get(notes_get).put(notes_save))
        .route("/api/upload", post(upload))
        .route("/uploads/:file", get(uploads_serve))
        .route("/api/detector-feedback", post(detector_feedback))
        .route("/api/config", get(config_get).put(config_put))
        .route("/api/config/test", post(config_test))
        .route("/api/setup/env", get(setup_env))
        .route("/api/server/version", get(server_version))
        .route("/api/server/update", post(server_update))
        .route("/api/server/restart", post(server_restart))
        .route("/api/images", get(images_list))
        .route("/api/images/pull", post(images_pull))
        .route("/api/images/commit", post(images_commit))
        .route("/api/images/delete", post(images_delete))
        .route("/api/chat/:id", get(chat_get).post(chat_send))
        .route("/api/chat/:id/events", get(chat_events))
        .route("/api/chat/:id/abort", post(chat_abort))
        .route("/api/hosts/:id/forwards", put(forwards_put))
        .route("/api/hosts/:id/group", post(host_group))
        .route("/api/hosts/:id/mcp", post(host_mcp))
        .route("/api/hosts/:id/exec", post(host_exec))
        // Group-proxy onboarding + CRUD (thin proxies to each group instance's management API).
        .route("/api/groups", post(groups_create))
        .route("/api/groups/:name", axum::routing::delete(groups_delete))
        .route("/api/groups/:name/accounts/login/start", post(group_login_start))
        .route("/api/groups/:name/accounts/login/status", get(group_login_status))
        .route("/api/groups/:name/accounts/login/complete", post(group_login_complete))
        .route("/api/groups/:name/accounts/delete", post(group_account_delete))
        .route("/api/usage/refresh", post(usage_refresh))
        // Group-proxy request router: reverse-proxy a clone's agent traffic to its group's
        // CLIProxyAPI instance. ANY method; registered BEFORE the SPA fallback below.
        .route("/cc/*rest", any(cc_proxy));

    // Frontend from the filesystem: a non-empty `static_dir` overrides (dev hot-reload
    // without a rebuild); otherwise the assets search path resolves it (the image's
    // /usr/local/share/rmng/static, else the repo dev build). The router is built once
    // at startup, so `static_dir` is restart-required by construction.
    let cfg_dir = app.config().static_dir;
    let dir = if !cfg_dir.is_empty() && Path::new(&cfg_dir).is_dir() {
        Some(std::path::PathBuf::from(&cfg_dir))
    } else {
        if !cfg_dir.is_empty() {
            tracing::warn!(
                "static_dir '{cfg_dir}' is not a directory; using the installed frontend"
            );
        }
        crate::assets::static_dir()
    };
    let routes = match dir {
        Some(dir) => {
            let index = dir.join("index.html");
            routes.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)))
        }
        None => {
            tracing::warn!(
                "no frontend found ({}/static or the dev build) — web UI disabled, API still up",
                crate::assets::INSTALL_DIR
            );
            routes.fallback(missing_frontend)
        }
    };

    // 64MB body cap (axum defaults to 2MB): the multipart routes carry full-resolution
    // clone screenshots — detector feedback evidence (~6MB PNG at 2560x1440) and note
    // uploads. LAN-only service; JSON routes are unaffected in practice.
    routes
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(app)
}

pub async fn serve(app: App) -> anyhow::Result<()> {
    let port = app.config().listen.web;
    let router = router(app);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("port 2 (web API + SSE + static) on http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

/// `GET /events` — three multiplexed streams on one connection:
///   - the persisted `ControlState` as the default (unnamed) event → the client's
///     `onmessage`: full snapshot on connect, then one frame per change;
///   - the volatile per-host CPU/RAM map as a named `stats` event → the client's
///     `addEventListener("stats")`: latest snapshot on connect, then one per poll tick;
///   - the volatile port-forward runtime map as a named `forwards` event → the client's
///     `addEventListener("forwards")`: snapshot on connect, then one per status change.
///
/// Stats and forwards ride separate SSE-only buses ([`crate::monitor::StatsBus`],
/// [`crate::forward::ForwardBus`]) so they never enter `ControlState` / `state.json`
/// (which persists on every mutation). Plus a named `ping` event every 15s (an
/// observable heartbeat the client's reconnect watchdog measures) and a 20s low-level
/// keep-alive comment.
async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (snapshot, rx) = app.store.subscribe();
    let state_initial = futures::stream::once(async move { Ok(Event::default().data(snapshot)) });
    let state_updates = BroadcastStream::new(rx).filter_map(|r| async move {
        match r {
            Ok(json) => Some(Ok(Event::default().data(json))),
            Err(_) => None, // lagged: next snapshot resyncs
        }
    });
    let state_stream = state_initial.chain(state_updates);

    let (stats_snapshot, stats_rx) = app.stats.subscribe();
    let stats_initial =
        futures::stream::once(
            async move { Ok(Event::default().event("stats").data(stats_snapshot)) },
        );
    let stats_updates = BroadcastStream::new(stats_rx).filter_map(|r| async move {
        match r {
            Ok(json) => Some(Ok(Event::default().event("stats").data(json))),
            Err(_) => None, // lagged: next tick resyncs
        }
    });
    let stats_stream = stats_initial.chain(stats_updates);

    let (fwd_snapshot, fwd_rx) = app.forwards.subscribe();
    let fwd_initial =
        futures::stream::once(
            async move { Ok(Event::default().event("forwards").data(fwd_snapshot)) },
        );
    let fwd_updates = BroadcastStream::new(fwd_rx).filter_map(|r| async move {
        match r {
            Ok(json) => Some(Ok(Event::default().event("forwards").data(json))),
            Err(_) => None,
        }
    });
    let fwd_stream = fwd_initial.chain(fwd_updates);

    // Observable heartbeat: a named `ping` event every 15s. Unlike the low-level keep-alive
    // *comment* below (which `EventSource` swallows silently), the client can see this — so
    // its watchdog can tell a wedged/half-open socket (pings stop arriving → reconnect)
    // apart from a merely idle fleet (pings keep arriving → stay put). First tick at 15s;
    // the initial snapshots above already prove liveness on connect.
    let heartbeat = futures::stream::unfold((), |()| async {
        tokio::time::sleep(Duration::from_secs(15)).await;
        Some((
            Ok::<Event, Infallible>(Event::default().event("ping").data("{}")),
            (),
        ))
    });

    Sse::new(futures::stream::select(
        state_stream,
        futures::stream::select(
            futures::stream::select(stats_stream, fwd_stream),
            heartbeat,
        ),
    ))
    .keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(20))
            .text("ping"),
    )
}

/// `GET /api/state` — the current [`ControlState`] as a single-shot snapshot (the same
/// JSON as the first default `/events` frame). For one-off readers — the `rmng` CLI,
/// scripts — that shouldn't have to open an SSE stream to see the fleet.
async fn state_get(State(app): State<App>) -> Json<ControlState> {
    Json(app.store.get())
}

#[derive(Deserialize)]
struct ActivateReq {
    #[serde(default)]
    id: Option<String>,
}

async fn activate(State(app): State<App>, Json(req): Json<ActivateReq>) -> Json<ControlState> {
    Json(app.store.mutate(|s| {
        // Switching to a clone clears its unread dot.
        if let Some(id) = req.id.as_deref() {
            if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                h.unread = false;
            }
        }
        s.selected = req.id;
    }))
}

#[derive(Deserialize)]
struct ReorderReq {
    order: Vec<String>,
}

async fn reorder(State(app): State<App>, Json(req): Json<ReorderReq>) -> Json<ControlState> {
    let next = app.store.mutate(|s| {
        let mut by_id: std::collections::HashMap<String, _> =
            s.hosts.drain(..).map(|h| (h.id.clone(), h)).collect();
        let mut out = Vec::with_capacity(by_id.len());
        for id in &req.order {
            if let Some(h) = by_id.remove(id) {
                out.push(h);
            }
        }
        out.extend(by_id.into_values());
        s.hosts = out;
    });
    Json(next)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForwardsPutReq {
    forwards: Vec<ForwardInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForwardInput {
    #[serde(default)]
    id: Option<String>,
    remote_port: u16,
    local_port: u16,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    label: Option<String>,
}

/// Validate a host's proposed forward set against the whole state and normalize it into
/// `PortForward`s (ids derived `f{local_port}`). Errors: port 0, duplicate local port
/// within the request, or a local port already claimed by a *different* host (the viewer
/// binds them all on one machine → the local-port space is global).
fn validate_forwards(
    state: &wire::ControlState,
    host_id: &str,
    inputs: Vec<ForwardInput>,
) -> Result<Vec<wire::PortForward>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    // Local ports claimed by OTHER hosts.
    let mut taken: std::collections::HashSet<u16> = state
        .hosts
        .iter()
        .filter(|h| h.id != host_id)
        .flat_map(|h| h.forwards.iter().map(|f| f.local_port))
        .collect();
    let mut out = Vec::with_capacity(inputs.len());
    for inp in inputs {
        if inp.remote_port == 0 || inp.local_port == 0 {
            return Err(bad("ports must be 1–65535".into()));
        }
        if !taken.insert(inp.local_port) {
            return Err(bad(format!(
                "local port {} is already in use",
                inp.local_port
            )));
        }
        out.push(wire::PortForward {
            id: inp.id.unwrap_or_else(|| format!("f{}", inp.local_port)),
            remote_port: inp.remote_port,
            local_port: inp.local_port,
            enabled: inp.enabled,
            label: inp.label,
        });
    }
    Ok(out)
}

/// `PUT /api/hosts/:id/forwards` — replace a host's forward rules. Validated
/// synchronously (returns 400 on conflict); persisted to `state.json`; the media plane
/// re-pushes the new set to the viewer off the store broadcast.
async fn forwards_put(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<ForwardsPutReq>,
) -> Result<Json<ControlState>, (StatusCode, String)> {
    let state = app.store.get();
    if !state.hosts.iter().any(|h| h.id == id) {
        return Err((StatusCode::NOT_FOUND, format!("no host '{id}'")));
    }
    let validated = validate_forwards(&state, &id, req.forwards)?;
    let next = app.store.mutate(|s| {
        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
            h.forwards = validated;
        }
    });
    Ok(Json(next))
}

// --- desktop proxy + exec (the `rmng desktop` / `rmng exec` backends) -------

/// `POST /api/hosts/:id/mcp` — proxy a desktop/window tool call to the clone's daemon MCP
/// (`:9004`). Body is [`wire::McpCallRequest`]; the response is the daemon's `content`
/// array. Unknown clone → 404; daemon unreachable / JSON-RPC error → 502. The daemon MCP
/// stays the single source of truth for the desktop tool schema — this handler is a thin
/// pass-through (`proxy_to_daemon`).
async fn host_mcp(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<wire::McpCallRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let host = host_by_id(&app, &id).ok_or((StatusCode::NOT_FOUND, format!("no host '{id}'")))?;
    let content = proxy_to_daemon(&app, &host, &req.tool, &req.args)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(content))
}

/// Proxy a desktop/window `tools/call` to a clone's clone-daemon MCP (dialed by container
/// name via Docker DNS — `App::dial_host`) and return its `result.content`. Moved here from
/// `mcp.rs` when the global MCP was retired; behavior is unchanged.
async fn proxy_to_daemon(
    app: &App,
    host: &wire::Host,
    name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
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
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("decoding clone-daemon MCP reply: {e}"))?;
    if let Some(err) = body.get("error") {
        return Err(format!("clone-daemon MCP error: {err}"));
    }
    body.get("result")
        .and_then(|r| r.get("content"))
        .cloned()
        .ok_or_else(|| "clone-daemon MCP result missing content".to_string())
}

/// `POST /api/hosts/:id/exec` — run a single non-interactive command inside the clone via
/// docker exec (`rmng exec`). Body is [`wire::ExecRequest`]; returns [`wire::ExecResult`]
/// (exit code + captured stdout/stderr). Empty argv → 400; unknown clone → 404; a bad
/// stdin payload → 400; a daemon/exec failure (e.g. container not running) → 502. Defaults
/// the run-as user to uid `1000` (the clone's agent user) when unset.
async fn host_exec(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<wire::ExecRequest>,
) -> Result<Json<wire::ExecResult>, (StatusCode, String)> {
    if req.cmd.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "cmd must not be empty".into()));
    }
    let host = host_by_id(&app, &id).ok_or((StatusCode::NOT_FOUND, format!("no host '{id}'")))?;
    let stdin = match &req.stdin_b64 {
        Some(b64) => Some(
            B64.decode(b64)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid stdinB64: {e}")))?,
        ),
        None => None,
    };
    let user = req.user.clone().unwrap_or_else(|| "1000".to_string());
    let result = app
        .docker
        .exec_capture(
            &host.id,
            &req.cmd,
            &user,
            req.workdir.as_deref(),
            &req.env,
            stdin.as_deref(),
        )
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(result))
}

/// `POST /api/clone` — start a clone from a source image. Body is one of:
///   `{ image, ticket }`                               — existing ticket (preset auto-selected
///                                                        by the ticket's labels)
///   `{ image, create: { team, title, description } }` — create a ticket first (preset required;
///                                                        its Linear key creates the issue)
///   `{ image, plain: { title, message } }`            — no ticket (preset required if any exist)
///   `{ image, hostname }`                             — raw clone under an exact hostname
///                                                        (fleet CLI; preset optional, no ticket)
/// plus optional `preset` (name; absent/"auto" = label auto-select in ticket mode) /
/// `group` (the account pool this clone's agents route through) / `agentInstructions` /
/// `claudeInstructions`. `image` is a clone-source image reference (e.g.
/// `pegasis0/rmng-template:latest`) from `GET /api/images`.
async fn clone(
    State(app): State<App>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let str_field = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::to_string);

    let image = str_field("image")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("body must include { image }".into()))?;
    let group = str_field("group");
    let agent_instructions = str_field("agentInstructions");
    let claude_instructions = str_field("claudeInstructions");
    let cfg = app.config();
    let prefix = cfg.docker.hostname_prefix.clone();

    // An explicitly chosen preset (by name); absent/"auto" means auto-select in
    // ticket mode and "required, so error" in plain/create mode (checked per mode).
    let explicit = match str_field("preset")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "auto")
    {
        Some(name) => Some(
            cfg.presets
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| bad(format!("unknown preset '{name}'")))?,
        ),
        None => None,
    };

    // suffix-aware display name (duplicate ticket → "title (a)").
    let derive = |app: &App, base: &str, title: &str| -> (String, String) {
        let hostname = jobs::next_free_hostname(app, base);
        let suffix = hostname.strip_prefix(base).unwrap_or("").to_string();
        let display = if suffix.is_empty() {
            title.to_string()
        } else {
            format!("{title} ({suffix})")
        };
        (hostname, display)
    };

    // Raw hostname clone (fleet CLI): the caller owns the exact hostname; no ticket, no
    // derived display name. A preset is optional — fleet workers usually need none; an
    // explicitly chosen one still applies its env + playbook append. Hostname validity +
    // uniqueness are gated by `start_clone`.
    if let Some(hostname) = str_field("hostname")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        let spec = CloneSpec {
            source_image: image,
            new_hostname: hostname,
            linear: None,
            group: group.clone(),
            first_message: None,
            agent_instructions,
            claude_instructions,
            preset_name: explicit.map(|p| p.name.clone()),
            env: explicit
                .map(crate::provision::preset_env_vars)
                .unwrap_or_default(),
            agent_playbook: compose_playbook(&cfg, explicit),
        };
        let op = jobs::start_clone(&app, spec).map_err(|e| bad(e.to_string()))?;
        return Ok(Json(json!({ "ok": true, "op": op })));
    }

    // Plain (no-ticket) clone: a preset must be picked whenever any are configured.
    if let Some(plain) = body.get("plain").filter(|v| v.is_object()) {
        let title = plain
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let message = plain
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if title.is_empty() {
            return Err(bad("plain.title is required".into()));
        }
        let env = match explicit {
            Some(p) => crate::provision::preset_env_vars(p),
            None if cfg.presets.is_empty() => Vec::new(),
            None => {
                return Err(bad(format!(
                    "a preset is required (configured: {})",
                    preset_names(&cfg)
                )));
            }
        };
        let (hostname, display) =
            derive(&app, &linear::plain_hostname_base(&prefix, &title), &title);
        let spec = CloneSpec {
            source_image: image,
            new_hostname: hostname,
            linear: Some(LinearMeta {
                display_name: Some(display),
                ..Default::default()
            }),
            group: group.clone(),
            first_message: Some(message).filter(|m| !m.is_empty()),
            agent_instructions,
            claude_instructions,
            preset_name: explicit.map(|p| p.name.clone()),
            env,
            agent_playbook: compose_playbook(&cfg, explicit),
        };
        let op = jobs::start_clone(&app, spec).map_err(|e| bad(e.to_string()))?;
        return Ok(Json(json!({ "ok": true, "op": op })));
    }

    // Ticket / create mode. `op_key` is the API key proven to reach the issue (used
    // for the state mutation); the preset drives the clone's env + LINEAR_API_KEY.
    let (issue, op_key, preset) = resolve_issue(&app, &cfg, explicit, &body)
        .await
        .map_err(bad)?;
    if let Err(e) = linear::ensure_in_progress(&app.http, &op_key, &issue).await {
        tracing::warn!("ensure_in_progress({}) failed: {e}", issue.identifier);
    }
    let base = linear::ticket_hostname_base(&prefix, &issue.identifier);
    let (hostname, display) = derive(&app, &base, &issue.title);
    let meta = LinearMeta {
        workspace: Some(issue.prefix.clone()),
        ticket: Some(issue.identifier.clone()),
        ticket_url: Some(issue.url.clone()),
        branch: Some(issue.branch.clone()),
        display_name: Some(display),
        label: issue.labels.first().cloned(),
    };
    let spec = CloneSpec {
        source_image: image,
        new_hostname: hostname,
        linear: Some(meta),
        group,
        first_message: None,
        agent_instructions,
        claude_instructions,
        preset_name: Some(preset.name.clone()),
        env: crate::provision::preset_env_vars(&preset),
        agent_playbook: compose_playbook(&cfg, Some(&preset)),
    };
    let op = jobs::start_clone(&app, spec).map_err(|e| bad(e.to_string()))?;
    Ok(Json(json!({ "ok": true, "op": op })))
}

fn preset_names(cfg: &wire::AppConfig) -> String {
    cfg.presets
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The effective agent playbook for a clone: the global `agentPlaybook` plus the preset's
/// optional append (after a blank line). Empty/whitespace preset field ⇒ global only. Mirrors
/// the wrapper's `[notes, procedure].filter(Boolean).join("\n\n")`.
pub(crate) fn compose_playbook(cfg: &wire::AppConfig, preset: Option<&wire::Preset>) -> String {
    let base = cfg.agent_playbook.trim();
    match preset
        .map(|p| p.agent_playbook.trim())
        .filter(|s| !s.is_empty())
    {
        Some(extra) => format!("{base}\n\n{extra}"),
        None => base.to_string(),
    }
}

/// Resolve the clone body to a Linear issue (create one, or fetch an existing), the
/// API key proven to reach it, and the preset that drives the clone's env.
async fn resolve_issue(
    app: &App,
    cfg: &wire::AppConfig,
    explicit: Option<&wire::Preset>,
    body: &serde_json::Value,
) -> Result<(linear::IssueInfo, String, wire::Preset), String> {
    if let Some(create) = body.get("create").filter(|v| v.is_object()) {
        let team = create.get("team").and_then(|v| v.as_str()).unwrap_or("");
        let title = create
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let description = create
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some(preset) = explicit else {
            return Err(
                "creating a ticket requires a preset (its Linear key creates the issue)".into(),
            );
        };
        if preset.linear_key.is_empty() {
            return Err(format!(
                "preset '{}' has no Linear API key — required to create a ticket",
                preset.name
            ));
        }
        let prefix = team.trim().to_ascii_lowercase();
        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("create.team must be a Linear team key like \"we\"".into());
        }
        if title.is_empty() {
            return Err("create.title is required".into());
        }
        let issue =
            linear::create_issue(&app.http, &preset.linear_key, &prefix, title, description)
                .await
                .map_err(|e| e.to_string())?;
        return Ok((issue, preset.linear_key.clone(), preset.clone()));
    }
    let ticket = body.get("ticket").and_then(|v| v.as_str()).unwrap_or("");
    if ticket.is_empty() {
        return Err("body must include { ticket } or { create }".into());
    }
    let r = linear::parse_ticket_ref(ticket).map_err(|e| e.to_string())?;
    // Key order: the explicitly chosen preset's key first, then every preset's key
    // in config order (fetch_issue_any dedups + skips blanks).
    let mut keys: Vec<&str> = Vec::new();
    if let Some(p) = explicit {
        keys.push(p.linear_key.as_str());
    }
    keys.extend(cfg.presets.iter().map(|p| p.linear_key.as_str()));
    let (issue, op_key) = linear::fetch_issue_any(&app.http, &keys, &r)
        .await
        .map_err(|e| e.to_string())?;
    let preset = match explicit {
        Some(p) => p.clone(),
        None => linear::pick_preset_by_prefix(&cfg.presets, &issue.prefix).cloned().ok_or_else(|| {
            format!(
                "no preset matches ticket {}'s team {} — pick a preset explicitly (configured: {})",
                issue.identifier,
                r.team_key,
                preset_names(cfg),
            )
        })?,
    };
    Ok((issue, op_key, preset))
}

// --- images (clone-source templates) ---------------------------------------

/// `GET /api/images` — the clone-source images (`rmng.image=1`), each with the names of
/// the managed containers created from it (`in_use_by`; container name == host id for
/// clones). Both halves come from the daemon — Docker, not `state.json`, knows which
/// containers reference which image. A daemon error surfaces as 502.
async fn images_list(
    State(app): State<App>,
) -> Result<Json<Vec<wire::ImageInfo>>, (StatusCode, String)> {
    let mut images = app
        .docker
        .list_rmng_images()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let containers = app
        .docker
        .list_managed_containers()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    fill_in_use_by(&mut images, &containers);
    Ok(Json(images))
}

/// Fill each image's `in_use_by` with the names of managed containers whose creation
/// image equals the image reference. Pure over (images, containers) so it's
/// unit-testable independent of the daemon.
fn fill_in_use_by(images: &mut [wire::ImageInfo], containers: &[crate::docker::ManagedContainer]) {
    for img in images.iter_mut() {
        img.in_use_by = containers
            .iter()
            .filter(|c| c.image == img.reference)
            .map(|c| c.name.clone())
            .collect();
    }
}

#[derive(Deserialize)]
struct PullReq {
    /// Registry reference to pull the template from. Absent/blank ⇒
    /// `config.docker.templateReference` (the wizard's default). The pulled image keeps this
    /// `repo:tag` as its clone-source reference — no local retag.
    #[serde(default)]
    reference: Option<String>,
}

/// `POST /api/images/pull` — pull the clone template from a registry (`reference`, default
/// `config.docker.templateReference`). The pulled image keeps its own `repo:tag` as the
/// clone-source reference (no retag). Returns the driving Operation (kind `pull`, which the
/// wizard watches for). Replaces the retired in-product `/api/images/bootstrap` build.
async fn images_pull(
    State(app): State<App>,
    Json(req): Json<PullReq>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    let reference = req
        .reference
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| app.config().docker.template_reference);
    jobs::start_pull(&app, &reference)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
struct CommitReq {
    /// Host id of the managed clone to commit.
    host: String,
    /// DNS-label image name — becomes the full repo of the committed image (`<name>:latest`).
    name: String,
}

/// `POST /api/images/commit` — commit a running clone to a new clone-source image
/// `<name>:latest` (the name is the full repo). Returns the driving Operation (kind `commit`).
async fn images_commit(
    State(app): State<App>,
    Json(req): Json<CommitReq>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    jobs::start_commit(&app, &req.host, &req.name)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
struct ImageDeleteReq {
    /// Image reference or id to remove.
    reference: String,
}

/// `POST /api/images/delete` — remove a clone-source image. 409 (Conflict) when the image is
/// still referenced: a managed container was created from it (per the daemon — the same
/// dependency that would make the daemon's own no-force removal fail, surfaced with the
/// container names), OR a running op (clone/commit) uses it.
async fn images_delete(
    State(app): State<App>,
    Json(req): Json<ImageDeleteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let reference = req.reference.trim();
    if reference.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "reference is required".into()));
    }
    let containers = app
        .docker
        .list_managed_containers()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let users: Vec<String> = containers
        .iter()
        .filter(|c| c.image == reference)
        .map(|c| c.name.clone())
        .collect();
    if !users.is_empty() {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "image is in use by {} clone(s): {}",
                users.len(),
                users.join(", ")
            ),
        ));
    }
    // A running clone-from-this-image or commit-to-this-reference also blocks removal.
    let busy = app.store.get().operations.iter().any(|o| {
        o.status == wire::OperationStatus::Running
            && (o.source.as_deref() == Some(reference) || o.target == reference)
    });
    if busy {
        return Err((
            StatusCode::CONFLICT,
            "image is in use by a running operation".into(),
        ));
    }
    app.docker
        .remove_image(reference)
        .await
        // The daemon's no-force removal 409s when a container still holds it; surface as 409.
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DeleteReq {
    id: String,
}

/// `POST /api/delete` — destroy a managed CT (or unregister a plain host).
async fn delete(
    State(app): State<App>,
    Json(req): Json<DeleteReq>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    // Drop the clone's group-proxy router key so a stale bearer can never route again.
    app.cliproxy.forget_host(&req.id);
    jobs::start_delete(&app, &req.id)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
struct LayoutActivateReq {
    name: String,
}

/// `POST /api/layout/activate` — make `name` the active layout preset and live-apply it
/// to every running clone (no session restart). Persists config, mirrors the active
/// name into ControlState (so all sidebars update over SSE), then pushes `SetMonitors`
/// to each daemon. Best-effort per clone; partial failures are reported.
async fn layout_activate(
    State(app): State<App>,
    Json(req): Json<LayoutActivateReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // 1. Validate + persist the active_layout.
    let mut cfg = app.config();
    if !cfg.layout_presets.iter().any(|p| p.name == req.name) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown layout preset '{}'", req.name),
        ));
    }
    cfg.active_layout = req.name.clone();
    crate::config::save(&cfg).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *app.cfg.write().unwrap() = cfg.clone();

    // 2. Mirror into ControlState for the sidebar (SSE broadcast).
    mirror_layout_to_state(&app);

    // 3. Live-apply to all running clones.
    let monitors = cfg.effective_monitors();
    let results = app.media.set_monitors_all(&monitors);
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for (id, r) in results {
        match r {
            Ok(()) => applied.push(id),
            Err(e) => errors.push(format!("{id}: {e}")),
        }
    }
    Ok(Json(
        serde_json::json!({ "ok": true, "applied": applied, "errors": errors }),
    ))
}

// --- notes + uploads (side stores, not in ControlState) --------------------

/// The notes editor's wire envelope, both directions: `{ "blocks": [...] }`. The
/// BlockNote document is stored on disk as a bare array; the `blocks` key is the HTTP
/// shape the frontend reads (`GET`) and writes (`PUT`) — keep them in lockstep.
#[derive(Deserialize)]
struct NotesBody {
    #[serde(default)]
    blocks: Vec<serde_json::Value>,
}

async fn notes_get(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
) -> Json<serde_json::Value> {
    let blocks = files::load_notes(&app.config().data_dir, &id).unwrap_or_default();
    Json(json!({ "blocks": blocks }))
}

async fn notes_save(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(body): Json<NotesBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    files::save_notes(&app.config().data_dir, &id, &body.blocks)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// `POST /api/upload` — multipart image upload; returns `{ url }`.
async fn upload(
    State(app): State<App>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        if field.name() == Some("file") {
            let ct = field.content_type().unwrap_or("").to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
            let url = files::save_upload(&app.config().data_dir, &ct, &bytes)
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
            return Ok(Json(json!({ "url": url })));
        }
    }
    Err((StatusCode::BAD_REQUEST, "no 'file' field".into()))
}

/// `GET /uploads/:file` — serve a stored upload by its generated name.
async fn uploads_serve(State(app): State<App>, AxPath(file): AxPath<String>) -> Response {
    match files::read_upload(&app.config().data_dir, &file) {
        Ok((bytes, ct)) => ([(header::CONTENT_TYPE, ct)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `POST /api/detector-feedback` — the clone's `clone-daemon report-detection` uploads a
/// wrong needs-human verdict (multipart) for tuning. The caller self-identifies with a
/// `clone` field (its hostname — clone IPs are dynamic Docker IPAM now, so there is no
/// source-IP mapping). Mirrors the old Bun route + `computer-use`'s payload.
async fn detector_feedback(
    State(app): State<App>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut clone_field: Option<String> = None;
    let mut fb = files::DetectorFeedback {
        kind: String::new(),
        mode: "screen".into(),
        detector_verdict: "working".into(),
        detector_reason: String::new(),
        actual_state: "working".into(),
        ignore_reasons: Vec::new(),
        criteria: String::new(),
        note: String::new(),
    };
    let mut screenshot: Option<Vec<u8>> = None;
    let mut capture: Option<String> = None;
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        match field.name().unwrap_or("") {
            "clone" => clone_field = field.text().await.ok().map(|s| s.trim().to_string()),
            "kind" => fb.kind = field.text().await.unwrap_or_default(),
            "mode" => fb.mode = field.text().await.unwrap_or_default(),
            "detectorVerdict" => fb.detector_verdict = field.text().await.unwrap_or_default(),
            "detectorReason" => fb.detector_reason = field.text().await.unwrap_or_default(),
            "actualState" => fb.actual_state = field.text().await.unwrap_or_default(),
            "criteria" => fb.criteria = field.text().await.unwrap_or_default(),
            "note" => fb.note = field.text().await.unwrap_or_default(),
            "ignoreReason" => {
                if let Ok(s) = field.text().await {
                    fb.ignore_reasons.push(s);
                }
            }
            "screenshot" => {
                screenshot = field.bytes().await.ok().map(|b| b.to_vec());
            }
            "capture" => {
                capture = field.text().await.ok();
            }
            _ => {}
        }
    }
    if fb.mode.is_empty() {
        fb.mode = "screen".into();
    }
    if fb.kind != "false-positive" && fb.kind != "false-negative" {
        return Err((
            StatusCode::BAD_REQUEST,
            "kind must be false-positive|false-negative".into(),
        ));
    }
    let clone = clone_field.filter(|c| !c.is_empty()).ok_or((
        StatusCode::BAD_REQUEST,
        "missing 'clone' field (the caller's clone id)".into(),
    ))?;
    let host_id = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == clone)
        .map(|h| h.id)
        .ok_or((StatusCode::NOT_FOUND, format!("no host named '{clone}'")))?;
    let id = files::save_detector_feedback(
        &app.config().data_dir,
        &host_id,
        &fb,
        screenshot.as_deref(),
        capture.as_deref(),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(
        "detector-feedback from {host_id}: {} [{}] (id {id})",
        fb.kind,
        fb.mode
    );
    Ok(Json(json!({ "ok": true, "id": id, "host": host_id })))
}

// --- config API (redacted read / validated write / live-apply) -------------

/// Copy the config's active layout + preset names into ControlState so the sidebar
/// switcher renders + highlights over the live `/events` SSE. Idempotent; call after any
/// change to `layout_presets` / `active_layout` and once at boot.
pub(crate) fn mirror_layout_to_state(app: &App) {
    let cfg = app.config();
    let active = cfg.active_layout.clone();
    let names: Vec<String> = cfg.layout_presets.iter().map(|p| p.name.clone()).collect();
    app.store.mutate(|s| {
        s.active_layout = active.clone();
        s.layout_preset_names = names.clone();
    });
}

/// `GET /api/config` — the redacted view (no plaintext secrets).
async fn config_get(State(app): State<App>) -> Json<AppConfigRedacted> {
    Json(app.config().redacted())
}

/// `PUT /api/config` — merge a partial update, persist (0600), apply live. The
/// response reports whether the change touched a restart-required setting so the UI
/// can prompt for a restart.
async fn config_put(
    State(app): State<App>,
    Json(incoming): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let old = app.config();
    let merged = config::merge_update(&old, incoming)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    config::save(&merged).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let restart_required = config::restart_required(&old, &merged);
    // Keep the DockerCtl's cached subnet in lockstep with the just-saved config BEFORE the
    // lazy `rmng` bridge is materialized (the wizard-finish flip below, and the first clone).
    // The ctl snapshots the subnet at boot from the DEFAULT config; without this, finishing
    // the wizard with a non-default subnet would create the bridge with that stale default,
    // then the next boot (ctl rebuilt from config) would reject the mismatched network.
    app.docker.set_subnet(&merged.docker.subnet);
    // A wizard-finish flip (`setupComplete` false → true) is where the lazy `rmng` network is
    // first materialized AND the control-server attaches itself at `.2` — both live in
    // `self_setup` (gated on `setup_complete`, which was still false at startup, so this flip
    // is the first run that does either). Re-running it here means a clone create later finds
    // the network up and the baked `.2` control URL already resolving. A failure is NON-fatal
    // (the config is already saved); `self_setup` records only a genuine network / self-attach
    // failure in `network_detail` (failing *required* env rows were already gated by the env
    // step and are not a wizard-finish failure), which we surface as `networkWarning` so the
    // wizard can show it (the network also gets re-ensured on the first clone).
    let mut network_warning: Option<String> = None;
    if !old.setup_complete && merged.setup_complete {
        // Bounded: the shared bollard client tolerates 1 h requests (commits); a wedged
        // daemon must not hang this PUT for that long.
        match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            app.docker.self_setup(true),
        )
        .await
        {
            Ok(report) => {
                if let Some(detail) = report.network_detail {
                    tracing::warn!(
                        "self_setup network/self-attach at wizard finish failed: {detail}"
                    );
                    network_warning = Some(detail);
                }
            }
            Err(_) => {
                let detail = "Docker self-setup timed out after 60s (daemon unresponsive?); \
                              the rmng network will be re-ensured on the first clone"
                    .to_string();
                tracing::warn!("{detail}");
                network_warning = Some(detail);
            }
        }
    }
    *app.cfg.write().unwrap() = merged.clone();
    // Propagate any SSH key change to the bastion + running clones immediately.
    if old.ssh.authorized_keys != merged.ssh.authorized_keys {
        // Bound the immediate push: apply_now does Docker calls to running clones; a wedged
        // daemon must not hang this PUT. The reconcile loop retries within ~10s regardless.
        if tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::ssh::apply_now(&app),
        )
        .await
        .is_err()
        {
            tracing::warn!("ssh apply_now timed out; reconcile loop will retry");
        }
    }
    // Keep the sidebar's live layout list/active marker in sync with the just-saved presets.
    mirror_layout_to_state(&app);
    let resp = ConfigPutResponse {
        restart_required,
        config: merged.redacted(),
    };
    let mut body = serde_json::to_value(&resp)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let (Some(obj), Some(w)) = (body.as_object_mut(), network_warning) {
        obj.insert("networkWarning".into(), json!(w));
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
struct TestReq {
    what: String,
}

/// `POST /api/config/test` — validate a setting from the UI. `"docker"` re-runs the Docker
/// self-setup probe and collapses the [`crate::docker::EnvReport`] into a single
/// `(ok, message)` verdict (the row-by-row breakdown is `GET /api/setup/env`).
async fn config_test(State(app): State<App>, Json(req): Json<TestReq>) -> Json<serde_json::Value> {
    let (ok, message) = match req.what.as_str() {
        "docker" => {
            let setup_complete = app.config().setup_complete;
            let report = app.docker.self_setup(setup_complete).await;
            collapse_env_report(&report)
        }
        other => (false, format!("unknown test '{other}'")),
    };
    Json(json!({ "ok": ok, "message": message }))
}

/// Collapse the self-setup report into a one-line `(ok, message)` verdict: `ok` iff nothing
/// required failed; the message names the first failing required check (or a success line).
fn collapse_env_report(report: &crate::docker::EnvReport) -> (bool, String) {
    let env = report.to_setup_env();
    let failing: Vec<&str> = env
        .rows
        .iter()
        .filter(|r| r.required && !r.ok)
        .map(|r| r.label.as_str())
        .collect();
    if failing.is_empty() {
        let ver = report.daemon_version.as_deref().unwrap_or("reachable");
        (true, format!("Docker {ver} — all required checks pass"))
    } else {
        (false, format!("failing: {}", failing.join(", ")))
    }
}

/// `GET /api/setup/env` — the setup wizard's environment preflight rows, from the cached
/// self-setup report (`SetupEnv`: daemon reachability, self-container detection, sock mount,
/// render node). The report is refreshed at startup + by `config_test("docker")`.
async fn setup_env(State(app): State<App>) -> Json<wire::SetupEnv> {
    Json(app.docker.env().await.to_setup_env())
}

/// `GET /api/server/version` — the control-server's own version + whether Hub has a newer
/// image (registry digest compare, no pull). Never 500s: registry/daemon failures land in
/// `UpdateStatus.error` so the UI always renders.
async fn server_version(State(app): State<App>) -> Json<wire::UpdateStatus> {
    let reference = app.config().docker.server_image;
    let self_id = app.docker.env().await.self_container;
    Json(
        app.docker
            .check_update(&reference, self_id.as_deref())
            .await,
    )
}

/// `POST /api/server/update` — pull `config.docker.serverImage` and swap the running
/// control-server container onto it. Returns the driving Operation (kind `update`); the
/// server restarts mid-op, and the rebooted server's reconcile finalizes it.
async fn server_update(State(app): State<App>) -> Result<Json<Operation>, (StatusCode, String)> {
    let reference = app.config().docker.server_image;
    jobs::start_update(&app, &reference)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// `POST /api/server/restart` — restart the control-server in place to apply restart-required
/// settings (ports / sockets / static dir / chroma), re-read from config.json on boot. The
/// response is sent before the daemon tears us down; the UI reconnects when we're back.
async fn server_restart(
    State(app): State<App>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let self_id = app.docker.env().await.self_container.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "not running as a container (dev mode) — restart manually".to_string(),
        )
    })?;
    let docker = app.docker.clone();
    // Spawn the restart so the HTTP response flushes to the client BEFORE the daemon stops us
    // (otherwise the browser sees a dropped connection instead of {ok:true}).
    tokio::spawn(async move {
        // Small delay to let the response return.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if let Err(e) = docker.restart_self(&self_id).await {
            tracing::error!(target: "update", "self-restart failed: {e:#}");
        }
    });
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- clone → group binding -------------------------------------------------

#[derive(Deserialize)]
struct HostGroupReq {
    /// The account pool this clone's agents route through, or `None`/absent to clear it
    /// (the clone runs with no inference until a group is bound again).
    #[serde(default)]
    group: Option<String>,
}

/// `POST /api/hosts/:id/group` — bind a clone to an account group (or clear the binding
/// with `{ "group": null }`). This is the sole account selection under the group-proxy
/// model: the `/cc` router maps the clone → its group → that group's CLIProxyAPI instance,
/// which owns intra-group account selection + OAuth refresh. No clone-side change is needed —
/// a group swap is a pure map update. Unknown host → 400; unmanaged row → 400; an unknown
/// group name → 400.
async fn host_group(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<HostGroupReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let host = host_by_id(&app, &id)
        .ok_or((StatusCode::BAD_REQUEST, format!("unknown host '{id}'")))?;
    if !host.managed {
        return Err((StatusCode::BAD_REQUEST, format!("'{id}' is not a managed clone")));
    }
    let group = match req.group.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => {
            if !app.config().groups.iter().any(|g| g.name == name) {
                return Err((StatusCode::BAD_REQUEST, format!("unknown group '{name}'")));
            }
            Some(name.to_string())
        }
        None => None,
    };
    let group_set = group.clone();
    app.store.mutate(|s| {
        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
            h.group = group_set;
        }
    });
    Ok(Json(json!({ "ok": true, "group": group })))
}

// --- per-host chat ---------------------------------------------------------

fn host_by_id(app: &App, id: &str) -> Option<wire::Host> {
    app.store.get().hosts.into_iter().find(|h| h.id == id)
}

/// `GET /api/chat/:id` — current chat snapshot (busy + activity + messages).
async fn chat_get(State(app): State<App>, AxPath(id): AxPath<String>) -> Response {
    let (snapshot, _rx) = crate::chat::subscribe(&app, &id);
    ([(header::CONTENT_TYPE, "application/json")], snapshot).into_response()
}

#[derive(Deserialize)]
struct ChatSendReq {
    text: String,
}

/// `POST /api/chat/:id` — send a message; the reply arrives over `/events`.
async fn chat_send(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<ChatSendReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let host = host_by_id(&app, &id)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("unknown host '{id}'")))?;
    crate::chat::send_chat(&app, &host, &req.text).map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/chat/:id/events` — per-host chat SSE (snapshot + on change).
async fn chat_events(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (snapshot, rx) = crate::chat::subscribe(&app, &id);
    let initial = futures::stream::once(async move { Ok(Event::default().data(snapshot)) });
    let updates = BroadcastStream::new(rx)
        .filter_map(|r| async move { r.ok().map(|json| Ok(Event::default().data(json))) });
    Sse::new(initial.chain(updates)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(20))
            .text("ping"),
    )
}

/// `POST /api/chat/:id/abort` — interrupt the in-flight turn.
async fn chat_abort(State(app): State<App>, AxPath(id): AxPath<String>) -> StatusCode {
    if let Some(host) = host_by_id(&app, &id) {
        crate::chat::abort_chat(&app, &host).await;
    }
    StatusCode::NO_CONTENT
}

// --- group-proxy request router (/cc) --------------------------------------

/// Headers we never forward verbatim in either direction: the framing/connection headers
/// hyper + reqwest recompute per hop (so passing them through would double-frame or lie about
/// length). `authorization` is handled separately by the router (dropped inbound, replaced
/// with the instance's inbound key). Everything else — crucially `content-type`, so
/// `text/event-stream` survives — passes through.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    let n = name.as_str();
    n.eq_ignore_ascii_case("host")
        || n.eq_ignore_ascii_case("connection")
        || n.eq_ignore_ascii_case("content-length")
        || n.eq_ignore_ascii_case("transfer-encoding")
}

/// `ANY /cc/*rest` — reverse-proxy a clone's agent traffic (Claude Code, Codex, OpenCode)
/// to its bound group's CLIProxyAPI instance on loopback. See the group-proxy plan
/// (`docs/superpowers/specs/2026-07-19-cliproxy-group-proxy-plan.md`).
///
/// 1. `Authorization: Bearer <per-clone key>` → host id (unknown/missing → 401).
/// 2. host id → `host.group` (none → 409 "clone has no group").
/// 3. group → instance loopback port + inbound key (missing/booting → 503; the agent retries).
/// 4. Forward the method + `*rest` path + query to `http://127.0.0.1:<port>/<rest>`, copying
///    every non-hop-by-hop header except `Authorization`, SETTING `Authorization: Bearer
///    <inbound_key>` and `X-Session-ID: <host_id>` (per-clone session stickiness), STREAMING
///    both request and response bodies so SSE (`text/event-stream`) is never buffered.
async fn cc_proxy(State(app): State<App>, req: Request) -> Response {
    let deny = |code: StatusCode, msg: &str| (code, msg.to_string()).into_response();

    // 1. Per-clone bearer key → host id.
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(host_id) = token.and_then(|t| app.cliproxy.host_for_token(t)) else {
        return deny(StatusCode::UNAUTHORIZED, "unknown or missing router bearer key");
    };

    // 2. Clone → group binding.
    let group = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == host_id)
        .and_then(|h| h.group);
    let Some(group) = group else {
        return deny(
            StatusCode::CONFLICT,
            "clone has no group (bind one in Settings before running an agent)",
        );
    };

    // 3. Group → loopback instance.
    let (Some(port), Some(inbound_key)) =
        (app.cliproxy.port_for(&group), app.cliproxy.inbound_key_for(&group))
    else {
        return deny(
            StatusCode::SERVICE_UNAVAILABLE,
            "group instance unavailable (still starting) — retry",
        );
    };

    // 4. Build + forward the streamed request.
    let (parts, body) = req.into_parts();
    let path = parts.uri.path();
    let rest = path.strip_prefix("/cc").filter(|s| !s.is_empty()).unwrap_or("/");
    let query = parts.uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("http://127.0.0.1:{port}{rest}{query}");

    let mut headers = reqwest::header::HeaderMap::new();
    for (k, v) in parts.headers.iter() {
        if is_hop_by_hop(k) || k == header::AUTHORIZATION {
            continue;
        }
        headers.insert(k.clone(), v.clone());
    }
    if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {inbound_key}")) {
        headers.insert(reqwest::header::AUTHORIZATION, val);
    }
    if let Ok(val) = reqwest::header::HeaderValue::from_str(&host_id) {
        headers.insert(HeaderName::from_static("x-session-id"), val);
    }

    let upstream_body = reqwest::Body::wrap_stream(body.into_data_stream());
    let resp = match app
        .http
        .request(parts.method, &url)
        .headers(headers)
        .body(upstream_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "router", "clone {host_id} → group {group} {url}: {e}");
            return deny(StatusCode::BAD_GATEWAY, "group instance request failed");
        }
    };

    tracing::debug!(target: "router", "clone {host_id} → group {group} {rest} → {}", resp.status());

    // 5. Stream the response back (status + headers + body), unbuffered.
    let status = resp.status();
    let mut builder = Response::builder().status(status);
    for (k, v) in resp.headers().iter() {
        if is_hop_by_hop(k) {
            continue;
        }
        builder = builder.header(k.clone(), v.clone());
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|e| {
            tracing::error!(target: "router", "building proxied response: {e}");
            StatusCode::BAD_GATEWAY.into_response()
        })
}

// --- group-proxy CRUD + onboarding -----------------------------------------

/// GET a group instance's management API (`{base}{path_and_query}`) with the plaintext
/// `X-Management-Key`, returning its JSON. 503 when the group has no instance meta yet; 502
/// on a dial/parse failure or a non-2xx from the instance.
/// Send a management request, retrying on a *connection* error for up to ~20s. A freshly
/// created group's CLIProxyAPI instance takes a couple seconds to spawn + bind (the supervisor
/// reconciles on a short interval), so the first onboarding call right after `POST /api/groups`
/// would otherwise hit connection-refused and surface a gateway error; this waits it out. Only
/// connect errors are retried — a real HTTP response (even non-2xx) returns immediately.
async fn mgmt_send_retry(
    http: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    secret: &str,
    body: Option<&serde_json::Value>,
) -> Result<reqwest::Response, (StatusCode, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let mut rb = http.request(method.clone(), url).header("X-Management-Key", secret);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        match rb.send().await {
            Ok(resp) => return Ok(resp),
            Err(e) if e.is_connect() && std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            }
            Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("group instance: {e}"))),
        }
    }
}

async fn mgmt_get_json(
    app: &App,
    group: &str,
    path_and_query: &str,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let (base, secret) = app
        .cliproxy
        .management(group)
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "group instance unavailable".into()))?;
    let resp = mgmt_send_retry(
        &app.http,
        reqwest::Method::GET,
        &format!("{base}{path_and_query}"),
        &secret,
        None,
    )
    .await?;
    mgmt_body(resp).await
}

/// Read a management-API response body as JSON, mapping a non-2xx status to a 502 with the
/// body text so the operator sees why onboarding failed.
async fn mgmt_body(resp: reqwest::Response) -> Result<serde_json::Value, (StatusCode, String)> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err((StatusCode::BAD_GATEWAY, format!("group instance {}: {text}", status.as_u16())));
    }
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({ "ok": true, "raw": text })))
}

#[derive(Deserialize)]
struct GroupCreateReq {
    name: String,
}

/// `POST /api/groups` — create an account group: validate the name, add a `wire::Group` to
/// `config.groups` if absent, persist the config, then `cliproxy::apply_now` so the
/// supervisor spawns its instance. Returns the redacted config.
async fn groups_create(
    State(app): State<App>,
    Json(req): Json<GroupCreateReq>,
) -> Result<Json<AppConfigRedacted>, (StatusCode, String)> {
    let name = req.name.trim().to_string();
    if !crate::cliproxy::safe_group(&name) {
        return Err((
            StatusCode::BAD_REQUEST,
            "group name must be 1–64 chars of [A-Za-z0-9._-]".into(),
        ));
    }
    let mut cfg = app.config();
    if !cfg.groups.iter().any(|g| g.name == name) {
        cfg.groups.push(wire::Group { name });
        config::save(&cfg).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        *app.cfg.write().unwrap() = cfg.clone();
        crate::cliproxy::apply_now(&app);
    }
    Ok(Json(cfg.redacted()))
}

/// `DELETE /api/groups/:name` — remove a group from `config.groups` + persist. The supervisor
/// stops its instance on the next reconcile; the on-disk `auth-dir` is left in place.
async fn groups_delete(
    State(app): State<App>,
    AxPath(name): AxPath<String>,
) -> Result<Json<AppConfigRedacted>, (StatusCode, String)> {
    let mut cfg = app.config();
    let before = cfg.groups.len();
    cfg.groups.retain(|g| g.name != name);
    if cfg.groups.len() != before {
        config::save(&cfg).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        *app.cfg.write().unwrap() = cfg.clone();
        crate::cliproxy::apply_now(&app);
    }
    Ok(Json(cfg.redacted()))
}

#[derive(Deserialize)]
struct LoginStartReq {
    provider: String,
}

/// `POST /api/groups/:name/accounts/login/start` — begin an OAuth login into the group's
/// instance. Proxies the instance's `{anthropic,codex,antigravity}-auth-url`; returns `{status, url,
/// state}`. The operator opens `url`, completes the login, and pastes the redirect back via
/// `login/complete`.
async fn group_login_start(
    State(app): State<App>,
    AxPath(name): AxPath<String>,
    Json(req): Json<LoginStartReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = match req.provider.trim().to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => "/anthropic-auth-url",
        "codex" | "openai" | "chatgpt" => "/codex-auth-url",
        "antigravity" | "gemini" | "google" => "/antigravity-auth-url",
        other => return Err((StatusCode::BAD_REQUEST, format!("unknown provider '{other}'"))),
    };
    let v = mgmt_get_json(&app, &name, path).await?;
    Ok(Json(v))
}

/// `GET /api/groups/:name/accounts/login/status?state=…` — poll the instance's
/// `get-auth-status` for an in-flight login and normalize the answer to a small stable shape
/// the browser branches on: `{"state":"pending"|"done"|"error","error"?:string}`.
///
/// CLIProxyAPI v7's `GetAuthStatus` (`internal/api/handlers/management/auth_files.go`) always
/// answers HTTP 200 with `{"status":"ok"|"wait"|"error","error"?:string}`: `wait` while the
/// background token exchange runs, `ok` once the credential is saved and the OAuth session is
/// marked `Completed`, and `error` (with a human message) for a failed / expired / unknown
/// session. `state` is required — an empty state makes the instance return a bare
/// `{"status":"ok"}` that would falsely read as done.
async fn group_login_status(
    State(app): State<App>,
    AxPath(name): AxPath<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let state = q.get("state").map(String::as_str).unwrap_or("");
    if state.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "state query param required".into()));
    }
    let enc = urlencode(state);
    let v = mgmt_get_json(&app, &name, &format!("/get-auth-status?state={enc}")).await?;
    let normalized = normalize_login_status(&v);
    // The moment the login completes the credential file is in the group's auth-dir — poke the
    // usage poller so the new account shows up in ~a second instead of at the next 600s poll.
    if normalized.get("state").and_then(serde_json::Value::as_str) == Some("done") {
        app.cliproxy.poke_usage();
    }
    Ok(Json(normalized))
}

/// Collapse CLIProxyAPI's `get-auth-status` body (`{"status":"ok"|"wait"|"error",…}`) into
/// `{"state":"pending"|"done"|"error","error"?:string}`. An unknown or missing `status` is
/// treated as `pending` so a surprising body keeps the poller waiting instead of falsely
/// completing the flow.
fn normalize_login_status(v: &serde_json::Value) -> serde_json::Value {
    match v.get("status").and_then(serde_json::Value::as_str) {
        Some("ok") => json!({ "state": "done" }),
        Some("error") => {
            let msg = v
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Authentication failed");
            json!({ "state": "error", "error": msg })
        }
        _ => json!({ "state": "pending" }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginCompleteReq {
    provider: String,
    #[serde(default)]
    redirect_url: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// `POST /api/groups/:name/accounts/login/complete` — finish the OAuth login by handing the
/// instance either the pasted `{redirectUrl}` or an explicit `{code, state}`. Proxies the
/// instance's `oauth-callback`.
async fn group_login_complete(
    State(app): State<App>,
    AxPath(name): AxPath<String>,
    Json(req): Json<LoginCompleteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (base, secret) = app
        .cliproxy
        .management(&name)
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "group instance unavailable".into()))?;
    let body = if let Some(redirect) = req.redirect_url.as_deref().filter(|s| !s.is_empty()) {
        json!({ "provider": req.provider, "redirect_url": redirect })
    } else if let (Some(code), Some(state)) = (
        req.code.as_deref().filter(|s| !s.is_empty()),
        req.state.as_deref().filter(|s| !s.is_empty()),
    ) {
        json!({ "provider": req.provider, "code": code, "state": state })
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "provide either redirectUrl or both code and state".into(),
        ));
    };
    let resp = mgmt_send_retry(
        &app.http,
        reqwest::Method::POST,
        &format!("{base}/oauth-callback"),
        &secret,
        Some(&body),
    )
    .await?;
    Ok(Json(mgmt_body(resp).await?))
}

#[derive(Deserialize)]
struct GroupAccountDeleteReq {
    file: String,
}

/// `POST /api/groups/:name/accounts/delete` — remove an authenticated account from the
/// group's instance by its `auth-dir` credential file name. Proxies the instance's
/// `DELETE /auth-files?name=<file>`.
async fn group_account_delete(
    State(app): State<App>,
    AxPath(name): AxPath<String>,
    Json(req): Json<GroupAccountDeleteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let file = req.file.trim();
    if file.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "file is required".into()));
    }
    let (base, secret) = app
        .cliproxy
        .management(&name)
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "group instance unavailable".into()))?;
    let resp = mgmt_send_retry(
        &app.http,
        reqwest::Method::DELETE,
        &format!("{base}/auth-files?name={}", urlencode(file)),
        &secret,
        None,
    )
    .await?;
    Ok(Json(mgmt_body(resp).await?))
}

/// `POST /api/usage/refresh` — trigger an immediate by-group usage poll (the manual refresh
/// button). Fire-and-forget: the poll runs in the background poller and the refreshed
/// `usage_groups` arrive over SSE within ~a second.
async fn usage_refresh(State(app): State<App>) -> impl IntoResponse {
    app.cliproxy.poke_usage();
    Json(json!({ "ok": true }))
}

/// Minimal percent-encoding for a query-string value (state tokens / file names). Encodes
/// everything outside the RFC 3986 unreserved set — no dependency for one small use.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::ManagedContainer;
    use wire::ImageInfo;

    fn image(reference: &str) -> ImageInfo {
        ImageInfo {
            id: format!("sha256:{reference}"),
            reference: reference.into(),
            size_bytes: 0,
            created_at: String::new(),
            base: false,
            created_from: None,
            in_use_by: Vec::new(),
        }
    }
    fn container_on(name: &str, image: &str) -> ManagedContainer {
        ManagedContainer {
            name: name.into(),
            image: image.into(),
            running: true,
        }
    }

    #[test]
    fn in_use_by_maps_containers_by_creation_image() {
        let mut images = vec![image("rmng/template:a"), image("rmng/template:b")];
        let containers = vec![
            container_on("h1", "rmng/template:a"),
            container_on("h2", "rmng/template:a"),
            container_on("h3", "rmng/template:b"),
            container_on("h5", "rmng/template:z"), // image not in the list → ignored
        ];
        fill_in_use_by(&mut images, &containers);
        assert_eq!(images[0].in_use_by, vec!["h1", "h2"]);
        assert_eq!(images[1].in_use_by, vec!["h3"]);
    }

    #[test]
    fn in_use_by_empty_when_no_containers_reference_it() {
        let mut images = vec![image("rmng/template:a")];
        let containers = vec![container_on("h1", "rmng/template:other")];
        fill_in_use_by(&mut images, &containers);
        assert!(images[0].in_use_by.is_empty());
    }

    // --- normalize_login_status: CLIProxyAPI v7 get-auth-status → {state, error?} ---

    #[test]
    fn login_status_wait_is_pending() {
        // `GetAuthStatus` returns `{"status":"wait"}` while the token exchange runs.
        let out = normalize_login_status(&json!({ "status": "wait" }));
        assert_eq!(out, json!({ "state": "pending" }));
    }

    #[test]
    fn login_status_ok_is_done() {
        // Session `Completed` → `{"status":"ok"}`.
        let out = normalize_login_status(&json!({ "status": "ok" }));
        assert_eq!(out, json!({ "state": "done" }));
    }

    #[test]
    fn login_status_error_surfaces_message() {
        // Errored/expired/unknown session → `{"status":"error","error":"..."}`.
        let out = normalize_login_status(
            &json!({ "status": "error", "error": "unknown or expired state" }),
        );
        assert_eq!(out, json!({ "state": "error", "error": "unknown or expired state" }));
    }

    #[test]
    fn login_status_error_without_message_falls_back() {
        let out = normalize_login_status(&json!({ "status": "error" }));
        assert_eq!(out, json!({ "state": "error", "error": "Authentication failed" }));
    }

    #[test]
    fn login_status_unknown_status_stays_pending() {
        // A surprising body must not falsely read as done.
        assert_eq!(
            normalize_login_status(&json!({ "foo": "bar" })),
            json!({ "state": "pending" }),
        );
        assert_eq!(
            normalize_login_status(&json!({ "status": "something-new" })),
            json!({ "state": "pending" }),
        );
    }

    // --- POST /api/images/pull (the endpoint that replaced /api/images/bootstrap) ---
    //
    // Handlers are called directly: `State`/`Json` are public tuple structs, so no HTTP
    // harness is needed. Docker is absent in tests, so a `start_pull` that passes the guards
    // spawns a background pull that fails later — but the test never yields (current-thread
    // runtime), so the returned op is observed before that task runs.

    use std::sync::Arc;

    fn test_app() -> App {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmng-web-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::state::StateStore::load(dir.join("state.json")).unwrap());
        let cfg = wire::AppConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        App::new(store, cfg)
    }

    #[tokio::test]
    async fn images_pull_registers_pull_op_and_defaults_reference() {
        let app = test_app();
        // `reference: None` → defaults to config.docker.template_reference; the op targets that
        // reference (no local name/retag).
        let op = images_pull(State(app.clone()), Json(PullReq { reference: None }))
            .await
            .unwrap()
            .0;
        assert_eq!(op.kind, wire::OperationKind::Pull);
        assert_eq!(op.target, app.config().docker.template_reference);
        assert_eq!(op.status, wire::OperationStatus::Running);
        // The op is registered in state (the wizard watches it over /events).
        assert!(app.store.get().operations.iter().any(|o| o.id == op.id));
    }

    #[tokio::test]
    async fn images_pull_rejects_duplicate_in_flight() {
        let app = test_app();
        // A blank reference defaults to config.docker.template_reference; the first pull
        // registers a Running op targeting that reference.
        let _first = images_pull(
            State(app.clone()),
            Json(PullReq {
                reference: Some("   ".into()),
            }),
        )
        .await
        .unwrap();
        // A second pull for the same reference is rejected while the first is in flight.
        let err = images_pull(
            State(app.clone()),
            Json(PullReq {
                reference: Some("pegasis0/rmng-template:latest".into()),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("already being pulled"), "msg: {}", err.1);
    }

    // --- GET /api/state (single-shot snapshot for the rmng CLI) ---

    #[tokio::test]
    async fn api_state_returns_current_snapshot() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(wire::Host {
                id: "w1".into(),
                host: "w1".into(),
                managed: true,
                ..Default::default()
            });
            s.selected = Some("w1".into());
        });
        let st = state_get(State(app.clone())).await.0;
        assert_eq!(st.hosts.len(), 1);
        assert_eq!(st.selected.as_deref(), Some("w1"));
    }

    #[tokio::test]
    async fn host_group_binds_and_clears_and_validates() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(wire::Host {
                id: "w1".into(),
                host: "w1".into(),
                managed: true,
                ..Default::default()
            });
        });
        *app.cfg.write().unwrap() = wire::AppConfig {
            groups: vec![wire::Group { name: "team".into() }],
            ..app.config()
        };

        // Bind to a known group.
        let resp = host_group(
            State(app.clone()),
            AxPath("w1".into()),
            Json(HostGroupReq { group: Some("team".into()) }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["group"], "team");
        let host = app.store.get().hosts.into_iter().find(|h| h.id == "w1").unwrap();
        assert_eq!(host.group.as_deref(), Some("team"));

        // Clear the binding with null.
        let _ = host_group(State(app.clone()), AxPath("w1".into()), Json(HostGroupReq { group: None }))
            .await
            .unwrap();
        let host = app.store.get().hosts.into_iter().find(|h| h.id == "w1").unwrap();
        assert!(host.group.is_none());

        // An unknown group name is a 400.
        let err = host_group(
            State(app.clone()),
            AxPath("w1".into()),
            Json(HostGroupReq { group: Some("nope".into()) }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("unknown group"), "msg: {}", err.1);
    }

    // --- POST /api/clone `hostname` mode (raw clone, fleet CLI) ---

    #[tokio::test]
    async fn clone_hostname_mode_registers_clone_op() {
        let app = test_app();
        let body =
            json!({ "image": "tmpl:latest", "hostname": "w-mod-claude", "group": "team" });
        let resp = clone(State(app.clone()), Json(body)).await.unwrap().0;
        assert_eq!(resp["ok"], true);
        let op: Operation = serde_json::from_value(resp["op"].clone()).unwrap();
        assert_eq!(op.kind, wire::OperationKind::Clone);
        assert_eq!(op.target, "w-mod-claude");
        assert_eq!(op.source.as_deref(), Some("tmpl:latest"));
        assert!(app.store.get().operations.iter().any(|o| o.id == op.id));
    }

    #[tokio::test]
    async fn clone_hostname_mode_rejects_bad_label() {
        let app = test_app();
        let body = json!({ "image": "tmpl:latest", "hostname": "Not A Label!" });
        let err = clone(State(app.clone()), Json(body)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("DNS label"), "msg: {}", err.1);
    }

    #[tokio::test]
    async fn clone_hostname_mode_rejects_unknown_preset() {
        let app = test_app();
        let body = json!({ "image": "tmpl:latest", "hostname": "w1", "preset": "nope" });
        let err = clone(State(app.clone()), Json(body)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("unknown preset"), "msg: {}", err.1);
    }

    // --- POST /api/hosts/:id/mcp + /exec (the rmng desktop / exec backends) ---

    #[tokio::test]
    async fn host_mcp_unknown_clone_is_404() {
        let app = test_app(); // no hosts registered
        let err = host_mcp(
            State(app.clone()),
            AxPath("ghost".into()),
            Json(wire::McpCallRequest {
                tool: "screenshot".into(),
                args: json!({}),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert!(err.1.contains("ghost"), "msg: {}", err.1);
    }

    #[tokio::test]
    async fn host_exec_unknown_clone_is_404() {
        let app = test_app();
        let err = host_exec(
            State(app.clone()),
            AxPath("ghost".into()),
            Json(wire::ExecRequest {
                cmd: vec!["echo".into(), "hi".into()],
                ..Default::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn host_exec_empty_cmd_is_400() {
        let app = test_app();
        let err = host_exec(
            State(app.clone()),
            AxPath("anything".into()),
            Json(wire::ExecRequest::default()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("cmd"), "msg: {}", err.1);
    }

    #[test]
    fn exec_request_result_map_camel_case() {
        // Request: snake-cased Rust fields serialize as the camelCase wire the CLI sends.
        let req = wire::ExecRequest {
            cmd: vec!["cat".into()],
            user: Some("1000".into()),
            workdir: Some("/tmp".into()),
            env: vec!["A=1".into()],
            stdin_b64: Some("aGk=".into()),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["cmd"][0], "cat");
        assert_eq!(v["stdinB64"], "aGk=");
        assert!(v.get("stdin_b64").is_none(), "must use camelCase key");
        // Result: exitCode maps back onto the i64 exit_code field.
        let res: wire::ExecResult =
            serde_json::from_str(r#"{ "exitCode": 3, "stdout": "out", "stderr": "err" }"#).unwrap();
        assert_eq!(res.exit_code, 3);
        assert_eq!(res.stdout, "out");
        assert_eq!(res.stderr, "err");
    }

    /// End-to-end through the real router: the notes editor saves with `PUT` and the
    /// `{ blocks }` envelope, and reads the same shape back. Goes over a live loopback
    /// socket (not a direct handler call) so it also pins the route *method* — a `POST`-
    /// only route would 405 the frontend's `PUT`, which is exactly the save bug.
    #[tokio::test]
    async fn notes_put_then_get_round_trips_over_http() {
        let app = test_app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        let base = format!("http://{addr}");
        let http = reqwest::Client::new();

        // A host with no notes yet reads back an empty `blocks` array (not a bare `[]`).
        let empty: serde_json::Value = http
            .get(format!("{base}/api/notes/h1"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(empty, serde_json::json!({ "blocks": [] }));

        // Save via PUT with the frontend's `{ blocks }` envelope → 204, no body.
        let doc = serde_json::json!({ "blocks": [{ "type": "paragraph", "id": "b1" }] });
        let put = http
            .put(format!("{base}/api/notes/h1"))
            .json(&doc)
            .send()
            .await
            .unwrap();
        assert_eq!(put.status(), reqwest::StatusCode::NO_CONTENT);

        // ...and the next GET returns exactly what was saved.
        let got: serde_json::Value = http
            .get(format!("{base}/api/notes/h1"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(got, doc);
    }

    // --- group-proxy router (/cc) token → host → group → port resolution ---

    fn cc_request(auth: Option<&str>) -> Request {
        let mut b = axum::http::Request::builder().method("POST").uri("/cc/v1/messages");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn cc_proxy_missing_or_unknown_bearer_is_401() {
        let app = test_app();
        // No Authorization header.
        assert_eq!(cc_proxy(State(app.clone()), cc_request(None)).await.status(), StatusCode::UNAUTHORIZED);
        // A bearer that maps to no host.
        assert_eq!(
            cc_proxy(State(app), cc_request(Some("Bearer nope"))).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn cc_proxy_clone_without_group_is_409() {
        let app = test_app();
        let key = app.cliproxy.mint_router_key("h1");
        app.store.mutate(|s| {
            s.hosts.push(wire::Host { id: "h1".into(), host: "h1".into(), managed: true, ..Default::default() });
        });
        let resp = cc_proxy(State(app), cc_request(Some(&format!("Bearer {key}")))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn cc_proxy_group_without_instance_is_503() {
        let app = test_app();
        let key = app.cliproxy.mint_router_key("h1");
        app.store.mutate(|s| {
            s.hosts.push(wire::Host {
                id: "h1".into(),
                host: "h1".into(),
                managed: true,
                group: Some("ghost".into()), // never provisioned → no port
                ..Default::default()
            });
        });
        let resp = cc_proxy(State(app), cc_request(Some(&format!("Bearer {key}")))).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn cc_proxy_resolves_group_then_dials_instance() {
        let app = test_app();
        let key = app.cliproxy.mint_router_key("h1");
        // Provision the group's instance meta (allocates a stable loopback port).
        app.cfg.write().unwrap().groups.push(wire::Group { name: "g".into() });
        crate::cliproxy::apply_now(&app);
        app.store.mutate(|s| {
            s.hosts.push(wire::Host {
                id: "h1".into(),
                host: "h1".into(),
                managed: true,
                group: Some("g".into()),
                ..Default::default()
            });
        });
        // Resolution passes token→host→group→port; the loopback instance isn't running in a
        // unit test, so the forward fails → 502. Proves the whole resolution chain wired up.
        let resp = cc_proxy(State(app), cc_request(Some(&format!("Bearer {key}")))).await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn is_hop_by_hop_matches_framing_headers_only() {
        assert!(is_hop_by_hop(&HeaderName::from_static("host")));
        assert!(is_hop_by_hop(&HeaderName::from_static("connection")));
        assert!(is_hop_by_hop(&HeaderName::from_static("content-length")));
        assert!(is_hop_by_hop(&HeaderName::from_static("transfer-encoding")));
        // Content-type + authorization are NOT framing headers (authorization is handled
        // separately by the router; content-type must survive for text/event-stream).
        assert!(!is_hop_by_hop(&HeaderName::from_static("content-type")));
        assert!(!is_hop_by_hop(&HeaderName::from_static("authorization")));
    }

    #[test]
    fn urlencode_escapes_reserved_chars() {
        assert_eq!(urlencode("abcABC123-_.~"), "abcABC123-_.~");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("claude-a@b.json"), "claude-a%40b.json");
    }

    /// Spin up `/events` and read the opening bytes. All three multiplexed streams send a
    /// snapshot on connect: the default (unnamed) `ControlState` frame plus the named
    /// `stats` and `forwards` snapshots. Guards the stream `select` wiring.
    #[tokio::test]
    async fn events_stream_multiplexes_snapshots_on_connect() {
        use futures::stream::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(test_app())).await.unwrap() });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/events"))
            .header("accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Ok(Some(chunk)) =
            tokio::time::timeout(Duration::from_secs(5), stream.next()).await
        {
            buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            let seen = buf.replace(' ', "");
            if seen.contains("event:stats") && seen.contains("event:forwards") {
                break;
            }
        }
        let seen = buf.replace(' ', "");
        assert!(seen.contains("data:"), "no default state frame in: {buf:?}");
        assert!(seen.contains("event:stats"), "no stats snapshot in: {buf:?}");
        assert!(seen.contains("event:forwards"), "no forwards snapshot in: {buf:?}");
    }

    /// The observable heartbeat: a named `ping` event arrives within the first interval.
    /// Distinct from the low-level keep-alive *comment* (`:ping`) — we assert the `event:`
    /// form so a comment can't satisfy it. Ignored by default: it waits ~15s for the first
    /// tick. Run with `cargo test -p control-server -- --ignored events_stream_emits_ping`.
    #[tokio::test]
    #[ignore = "waits ~15s for the first server heartbeat tick"]
    async fn events_stream_emits_ping_heartbeat() {
        use futures::stream::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(test_app())).await.unwrap() });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/events"))
            .header("accept", "text/event-stream")
            .send()
            .await
            .unwrap();

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Ok(Some(chunk)) =
            tokio::time::timeout(Duration::from_secs(18), stream.next()).await
        {
            buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if buf.replace(' ', "").contains("event:ping") {
                break;
            }
        }
        assert!(
            buf.replace(' ', "").contains("event:ping"),
            "no ping heartbeat event within ~18s: {buf:?}"
        );
    }
}

#[cfg(test)]
mod forwards_validation_tests {
    use super::*;
    use wire::{ControlState, Host};

    fn state_with(hosts: Vec<Host>) -> ControlState {
        ControlState {
            hosts,
            ..Default::default()
        }
    }

    fn host(id: &str) -> Host {
        Host {
            id: id.into(),
            host: id.into(),
            ..Default::default()
        }
    }

    fn input(remote: u16, local: u16) -> ForwardInput {
        ForwardInput {
            id: None,
            remote_port: remote,
            local_port: local,
            enabled: true,
            label: None,
        }
    }

    #[test]
    fn assigns_ids_from_local_port() {
        let st = state_with(vec![host("a")]);
        let out = validate_forwards(&st, "a", vec![input(3000, 8080)]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "f8080");
        assert_eq!(out[0].remote_port, 3000);
    }

    #[test]
    fn rejects_zero_port() {
        let st = state_with(vec![host("a")]);
        let err = validate_forwards(&st, "a", vec![input(0, 8080)]).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_duplicate_local_within_request() {
        let st = state_with(vec![host("a")]);
        let err = validate_forwards(&st, "a", vec![input(1, 8080), input(2, 8080)]).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_local_port_used_by_another_host() {
        let mut other = host("b");
        other.forwards = vec![wire::PortForward {
            id: "f8080".into(),
            remote_port: 9,
            local_port: 8080,
            enabled: true,
            label: None,
        }];
        let st = state_with(vec![host("a"), other]);
        let err = validate_forwards(&st, "a", vec![input(3000, 8080)]).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
}

#[cfg(test)]
mod playbook_tests {
    use super::*;

    fn cfg_with(global: &str) -> wire::AppConfig {
        wire::AppConfig {
            agent_playbook: global.into(),
            ..Default::default()
        }
    }
    fn preset_with(pb: &str) -> wire::Preset {
        wire::Preset {
            name: "p".into(),
            agent_playbook: pb.into(),
            ..Default::default()
        }
    }

    #[test]
    fn global_only_when_no_preset() {
        assert_eq!(compose_playbook(&cfg_with("BASE"), None), "BASE");
    }

    #[test]
    fn global_only_when_preset_field_empty() {
        assert_eq!(
            compose_playbook(&cfg_with("BASE"), Some(&preset_with("  "))),
            "BASE"
        );
    }

    #[test]
    fn appends_preset_after_global_with_blank_line() {
        assert_eq!(
            compose_playbook(&cfg_with("BASE"), Some(&preset_with("EXTRA"))),
            "BASE\n\nEXTRA"
        );
    }
}
