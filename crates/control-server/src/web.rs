//! Port 2 — the web API + SSE + static frontend. Phase 1 + the Phase-2 clone/
//! delete surface; the rest (Linear/Claude/chat/config/…) lands as those modules
//! are ported.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{ConnectInfo, Multipart, Path as AxPath, State},
    http::{StatusCode, Uri, header},
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::stream::{Stream, StreamExt};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

/// The built frontend (`bun run build` → `frontend/build/client`), embedded into
/// the binary so the server is a single self-contained artifact. `build.rs`
/// guarantees the folder exists (placeholder if the frontend wasn't built).
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../frontend/build/client"]
struct Frontend;

/// Serve an embedded asset; SPA fallback to `index.html` for unknown paths.
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(f) = Frontend::get(path) {
        return ([(header::CONTENT_TYPE, f.metadata.mimetype())], f.data.into_owned()).into_response();
    }
    match Frontend::get("index.html") {
        Some(f) => ([(header::CONTENT_TYPE, "text/html")], f.data.into_owned()).into_response(),
        None => (StatusCode::NOT_FOUND, "frontend not embedded").into_response(),
    }
}
use wire::{AppConfigRedacted, ControlState, Operation};

use crate::app::App;
use crate::config;
use crate::files;
use crate::jobs::{self, CloneSpec, LinearMeta};
use crate::linear;

pub fn router(app: App) -> Router {
    let routes = Router::new()
        .route("/events", get(events))
        .route("/api/activate", post(activate))
        .route("/api/reorder", post(reorder))
        .route("/api/clone", post(clone))
        .route("/api/clone/redeploy", post(clone_redeploy))
        .route("/api/monitors/apply", post(monitors_apply))
        .route("/api/delete", post(delete))
        .route("/api/notes/:id", get(notes_get).post(notes_save))
        .route("/api/upload", post(upload))
        .route("/uploads/:file", get(uploads_serve))
        .route("/api/detector-feedback", post(detector_feedback))
        .route("/api/config", get(config_get).put(config_put))
        .route("/api/config/test", post(config_test))
        .route("/api/template/bootstrap", post(template_bootstrap))
        .route("/api/claude/import", post(claude_import))
        .route("/api/claude/refresh", post(claude_refresh))
        .route("/api/claude/recommended", get(claude_recommended))
        .route("/api/claude/swap", post(claude_swap))
        .route("/api/chat/:id", get(chat_get).post(chat_send))
        .route("/api/chat/:id/events", get(chat_events))
        .route("/api/chat/:id/abort", post(chat_abort));

    // Frontend: embedded by default (self-contained binary). `RMNG_STATIC_DIR`
    // serves from disk instead, for dev hot-reload without a rebuild.
    let routes = match std::env::var("RMNG_STATIC_DIR") {
        Ok(dir) if !dir.is_empty() => {
            let index = Path::new(&dir).join("index.html");
            routes.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)))
        }
        _ => routes.fallback(static_handler),
    };

    routes.layer(TraceLayer::new_for_http()).with_state(app)
}

pub async fn serve(app: App) -> anyhow::Result<()> {
    let port = app.config().listen.web;
    let router = router(app);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("port 2 (web API + SSE + static) on http://{addr}");
    // ConnectInfo so /api/detector-feedback can map the caller's source IP → clone.
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

/// `GET /events` — full `ControlState` on connect, then on every change; 20s ping.
async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (snapshot, rx) = app.store.subscribe();
    let initial = futures::stream::once(async move { Ok(Event::default().data(snapshot)) });
    let updates = BroadcastStream::new(rx).filter_map(|r| async move {
        match r {
            Ok(json) => Some(Ok(Event::default().data(json))),
            Err(_) => None, // lagged: next snapshot resyncs
        }
    });
    Sse::new(initial.chain(updates))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(20)).text("ping"))
}

#[derive(Deserialize)]
struct ActivateReq {
    #[serde(default)]
    id: Option<String>,
}

async fn activate(State(app): State<App>, Json(req): Json<ActivateReq>) -> Json<ControlState> {
    Json(app.store.mutate(|s| s.selected = req.id))
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

/// `POST /api/clone` — start a CoW clone. Body is one of:
///   `{ source, ticket }`                                    — existing ticket
///   `{ source, create: { workspace, title, description } }` — create a ticket first
///   `{ source, plain: { title, message } }`                 — no ticket, just a title
/// plus optional `claudeAccount` / `agentInstructions` / `claudeInstructions`.
async fn clone(
    State(app): State<App>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let str_field = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::to_string);

    let source = str_field("source").filter(|s| !s.is_empty()).ok_or_else(|| bad("body must include { source }".into()))?;
    let claude_account = str_field("claudeAccount");
    let agent_instructions = str_field("agentInstructions");
    let claude_instructions = str_field("claudeInstructions");
    let cfg = app.config();
    let prefix = cfg.proxmox.hostname_prefix.clone();

    // Resolve the chosen env-var preset (by name) to its vars; written into the clone's
    // session env at creation. Empty/absent = no preset; an unknown name is an error.
    let env_vars = match str_field("envPreset").filter(|s| !s.is_empty()) {
        Some(name) => cfg
            .env_presets
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.vars.clone())
            .ok_or_else(|| bad(format!("unknown env preset '{name}'")))?,
        None => Vec::new(),
    };

    // suffix-aware display name (duplicate ticket → "title (a)").
    let derive = |app: &App, base: &str, title: &str| -> (String, String) {
        let hostname = jobs::next_free_hostname(app, base);
        let suffix = hostname.strip_prefix(base).unwrap_or("").to_string();
        let display = if suffix.is_empty() { title.to_string() } else { format!("{title} ({suffix})") };
        (hostname, display)
    };

    // Plain (no-ticket) clone.
    if let Some(plain) = body.get("plain").filter(|v| v.is_object()) {
        let title = plain.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let message = plain.get("message").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if title.is_empty() {
            return Err(bad("plain.title is required".into()));
        }
        let (hostname, display) = derive(&app, &linear::plain_hostname_base(&prefix, &title), &title);
        let spec = CloneSpec {
            source_id: source,
            new_hostname: hostname,
            linear: Some(LinearMeta { display_name: Some(display), ..Default::default() }),
            claude_account,
            first_message: Some(message).filter(|m| !m.is_empty()),
            agent_instructions,
            claude_instructions,
            env: env_vars,
        };
        let op = jobs::start_clone(&app, spec).map_err(|e| bad(e.to_string()))?;
        return Ok(Json(json!({ "ok": true, "op": op })));
    }

    // Ticket / create mode.
    let issue = resolve_issue(&app, &cfg.linear, &body).await.map_err(bad)?;
    if let Err(e) = linear::ensure_in_progress(&app.http, &cfg.linear, &issue).await {
        tracing::warn!("ensure_in_progress({}) failed: {e}", issue.identifier);
    }
    let base = linear::ticket_hostname_base(&prefix, &issue.identifier);
    let (hostname, display) = derive(&app, &base, &issue.title);
    let meta = LinearMeta {
        workspace: Some(issue.prefix),
        ticket: Some(issue.identifier.clone()),
        ticket_url: Some(issue.url.clone()),
        branch: Some(issue.branch.clone()),
        display_name: Some(display),
        label: Some(issue.label.clone()),
    };
    let spec = CloneSpec {
        source_id: source,
        new_hostname: hostname,
        linear: Some(meta),
        claude_account,
        first_message: None,
        agent_instructions,
        claude_instructions,
        env: env_vars,
    };
    let op = jobs::start_clone(&app, spec).map_err(|e| bad(e.to_string()))?;
    Ok(Json(json!({ "ok": true, "op": op })))
}

/// Resolve the clone body to a Linear issue (create one, or fetch an existing).
async fn resolve_issue(
    app: &App,
    lcfg: &wire::LinearConfig,
    body: &serde_json::Value,
) -> Result<linear::IssueInfo, String> {
    if let Some(create) = body.get("create").filter(|v| v.is_object()) {
        let workspace = create.get("workspace").and_then(|v| v.as_str()).unwrap_or("");
        let title = create.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
        let description = create.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let prefix = linear::prefix_from_str(workspace)
            .ok_or_else(|| "create.workspace must be one of we, dev, hh, per".to_string())?;
        if title.is_empty() {
            return Err("create.title is required".into());
        }
        return linear::create_issue(&app.http, lcfg, prefix, title, description)
            .await
            .map_err(|e| e.to_string());
    }
    let ticket = body.get("ticket").and_then(|v| v.as_str()).unwrap_or("");
    if ticket.is_empty() {
        return Err("body must include { ticket } or { create }".into());
    }
    let r = linear::parse_ticket_ref(ticket).map_err(|e| e.to_string())?;
    linear::fetch_issue(&app.http, lcfg, &r).await.map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct BootstrapReq {
    hostname: String,
}

/// `POST /api/template/bootstrap` — build a template/clone from a base image.
async fn template_bootstrap(
    State(app): State<App>,
    Json(req): Json<BootstrapReq>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    jobs::start_bootstrap(&app, &req.hostname)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
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
    jobs::start_delete(&app, &req.id)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RedeployReq {
    id: String,
    #[serde(default)]
    daemon_only: bool,
}

/// `POST /api/clone/redeploy` — hot-swap a clone's `clone-daemon` (+ `agent-wrapper`
/// unless `daemonOnly`) binaries from the embedded copies, without reprovisioning.
async fn clone_redeploy(
    State(app): State<App>,
    Json(req): Json<RedeployReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let host = app.store.get().hosts.iter().find(|h| h.id == req.id).cloned();
    let host = host.ok_or_else(|| (StatusCode::NOT_FOUND, format!("unknown host '{}'", req.id)))?;
    let ctid = host
        .ctid
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("'{}' has no container to redeploy", req.id)))?;
    let cfg = app.config();
    crate::orchestrate::redeploy_clone(&cfg, ctid, "rmng", req.daemon_only, |step, msg| {
        tracing::info!("redeploy CT {ctid} {step}: {msg}");
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /api/monitors/apply` — apply the saved monitor layout to every running clone
/// (rewrites its `RMNG_MONITORS` + restarts its GNOME session + daemon). Restarts the
/// clones' desktops, so it's an explicit button rather than part of Save.
async fn monitors_apply(State(app): State<App>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = app.config();
    let clones: Vec<(String, u32)> =
        app.store.get().hosts.iter().filter_map(|h| h.ctid.map(|c| (h.id.clone(), c))).collect();
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for (id, ctid) in clones {
        match crate::orchestrate::apply_monitors(&cfg, ctid, "rmng", |step, msg| {
            tracing::info!("apply-monitors CT {ctid} {step}: {msg}");
        })
        .await
        {
            Ok(()) => applied.push(id),
            Err(e) => errors.push(format!("{id}: {e}")),
        }
    }
    if applied.is_empty() && !errors.is_empty() {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, errors.join("; ")));
    }
    Ok(Json(serde_json::json!({ "ok": true, "applied": applied, "errors": errors })))
}

// --- notes + uploads (side stores, not in ControlState) --------------------

async fn notes_get(State(app): State<App>, AxPath(id): AxPath<String>) -> Json<Vec<serde_json::Value>> {
    Json(files::load_notes(&app.config().data_dir, &id).unwrap_or_default())
}

async fn notes_save(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(blocks): Json<Vec<serde_json::Value>>,
) -> Result<StatusCode, (StatusCode, String)> {
    files::save_notes(&app.config().data_dir, &id, &blocks)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// `POST /api/upload` — multipart image upload; returns `{ url }`.
async fn upload(
    State(app): State<App>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    while let Some(field) = mp.next_field().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))? {
        if field.name() == Some("file") {
            let ct = field.content_type().unwrap_or("").to_string();
            let bytes = field.bytes().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
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
/// wrong needs-human verdict (multipart) for tuning. The caller is mapped to its clone by
/// source IP. Mirrors the old Bun route + `computer-use`'s payload.
async fn detector_feedback(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let peer_ip = peer.ip().to_string();
    let host_id = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.host == peer_ip)
        .map(|h| h.id)
        .ok_or((StatusCode::NOT_FOUND, format!("no host matches source ip {peer_ip}")))?;

    let mut fb = files::DetectorFeedback {
        kind: String::new(),
        detector_verdict: "working".into(),
        detector_reason: String::new(),
        actual_state: "working".into(),
        ignore_reasons: Vec::new(),
        note: String::new(),
    };
    let mut screenshot: Option<Vec<u8>> = None;
    while let Some(field) = mp.next_field().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))? {
        match field.name().unwrap_or("") {
            "kind" => fb.kind = field.text().await.unwrap_or_default(),
            "detectorVerdict" => fb.detector_verdict = field.text().await.unwrap_or_default(),
            "detectorReason" => fb.detector_reason = field.text().await.unwrap_or_default(),
            "actualState" => fb.actual_state = field.text().await.unwrap_or_default(),
            "note" => fb.note = field.text().await.unwrap_or_default(),
            "ignoreReason" => {
                if let Ok(s) = field.text().await {
                    fb.ignore_reasons.push(s);
                }
            }
            "screenshot" => {
                screenshot = field.bytes().await.ok().map(|b| b.to_vec());
            }
            _ => {}
        }
    }
    if fb.kind != "false-positive" && fb.kind != "false-negative" {
        return Err((StatusCode::BAD_REQUEST, "kind must be false-positive|false-negative".into()));
    }
    let id = files::save_detector_feedback(&app.config().data_dir, &host_id, &fb, screenshot.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!("detector-feedback from {host_id} ({peer_ip}): {} (id {id})", fb.kind);
    Ok(Json(json!({ "ok": true, "id": id, "host": host_id })))
}

// --- config API (redacted read / validated write / live-apply) -------------

/// `GET /api/config` — the redacted view (no plaintext secrets).
async fn config_get(State(app): State<App>) -> Json<AppConfigRedacted> {
    Json(app.config().redacted())
}

/// `PUT /api/config` — merge a partial update, persist (0600), apply live.
async fn config_put(
    State(app): State<App>,
    Json(incoming): Json<serde_json::Value>,
) -> Result<Json<AppConfigRedacted>, (StatusCode, String)> {
    let merged = config::merge_update(&app.config(), incoming)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    config::save(&merged).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *app.cfg.write().unwrap() = merged.clone();
    Ok(Json(merged.redacted()))
}

#[derive(Deserialize)]
struct TestReq {
    what: String,
}

/// `POST /api/config/test` — validate a setting from the UI.
async fn config_test(State(app): State<App>, Json(req): Json<TestReq>) -> Json<serde_json::Value> {
    let cfg = app.config();
    let (ok, message) = match req.what.as_str() {
        "proxmox" => test_ssh(&cfg.proxmox.ssh).await,
        other => (false, format!("unknown test '{other}'")),
    };
    Json(json!({ "ok": ok, "message": message }))
}

async fn test_ssh(target: &str) -> (bool, String) {
    if target.is_empty() {
        return (false, "proxmox.ssh is not set".into());
    }
    match tokio::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", target, "true"])
        .output()
        .await
    {
        Ok(o) if o.status.success() => (true, "reachable".into()),
        Ok(o) => (false, String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => (false, e.to_string()),
    }
}

// --- Claude accounts -------------------------------------------------------

/// `POST /api/claude/import` — import accounts from the claude-swap mount.
async fn claude_import(State(app): State<App>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let n = tokio::task::spawn_blocking(move || crate::claude::import_from_swap(&app))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(json!({ "ok": true, "imported": n })))
}

/// `POST /api/claude/refresh` — force one usage poll now.
async fn claude_refresh(State(app): State<App>) -> Json<serde_json::Value> {
    let any429 = crate::claude::poll_once(&app).await.unwrap_or(false);
    Json(json!({ "ok": true, "rateLimited": any429 }))
}

/// `GET /api/claude/recommended` — the account the clone dialog should pre-select.
async fn claude_recommended(State(app): State<App>) -> Json<serde_json::Value> {
    Json(json!({ "email": crate::claude::recommend(&app).map(|a| a.email) }))
}

#[derive(Deserialize)]
struct SwapReq {
    host: String,
    /// Account email, or "auto".
    account: String,
}

/// `POST /api/claude/swap` — hot-swap a running clone's Claude token.
async fn claude_swap(
    State(app): State<App>,
    Json(req): Json<SwapReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let host = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == req.host)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("unknown host '{}'", req.host)))?;
    let account = crate::claude::resolve_clone_account(&app, Some(&req.account))
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "no clone accounts configured".into()))?;
    crate::claude::apply_clone_token(&host, &account.long_lived_token)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (id, email) = (host.id.clone(), account.email.clone());
    app.store.mutate(|s| {
        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
            h.claude_account_email = Some(email);
        }
    });
    Ok(Json(json!({ "ok": true, "account": account.email })))
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
    let host = host_by_id(&app, &id).ok_or_else(|| (StatusCode::BAD_REQUEST, format!("unknown host '{id}'")))?;
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
    let updates = BroadcastStream::new(rx).filter_map(|r| async move {
        r.ok().map(|json| Ok(Event::default().data(json)))
    });
    Sse::new(initial.chain(updates))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(20)).text("ping"))
}

/// `POST /api/chat/:id/abort` — interrupt the in-flight turn.
async fn chat_abort(State(app): State<App>, AxPath(id): AxPath<String>) -> StatusCode {
    if let Some(host) = host_by_id(&app, &id) {
        crate::chat::abort_chat(&app, &host).await;
    }
    StatusCode::NO_CONTENT
}
