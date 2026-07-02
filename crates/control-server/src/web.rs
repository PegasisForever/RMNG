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
use wire::{AppConfigRedacted, ConfigPutResponse, ControlState, Operation};

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
        .route("/api/setup/env", get(setup_env))
        .route("/api/images", get(images_list))
        .route("/api/images/bootstrap", post(images_bootstrap))
        .route("/api/images/commit", post(images_commit))
        .route("/api/images/delete", post(images_delete))
        .route("/api/claude/import/check", post(claude_import_check))
        .route("/api/claude/import", post(claude_import))
        .route("/api/claude/refresh", post(claude_refresh))
        .route("/api/claude/recommended", get(claude_recommended))
        .route("/api/claude/swap", post(claude_swap))
        .route("/api/claude/rotate", post(claude_rotate))
        .route("/api/chat/:id", get(chat_get).post(chat_send))
        .route("/api/chat/:id/events", get(chat_events))
        .route("/api/chat/:id/abort", post(chat_abort));

    // Frontend: embedded by default (self-contained binary). A non-empty
    // `static_dir` serves from disk instead, for dev hot-reload without a rebuild.
    // The router is built once at startup, so `static_dir` is restart-required by
    // construction — no live re-read needed.
    let dir = app.config().static_dir;
    let routes = if !dir.is_empty() && Path::new(&dir).is_dir() {
        let index = Path::new(&dir).join("index.html");
        routes.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)))
    } else {
        if !dir.is_empty() {
            tracing::warn!("static_dir '{dir}' is not a directory; serving the embedded frontend");
        }
        routes.fallback(static_handler)
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

/// `POST /api/clone` — start a clone from a source image. Body is one of:
///   `{ image, ticket }`                               — existing ticket (preset auto-selected
///                                                        by the ticket's labels)
///   `{ image, create: { team, title, description } }` — create a ticket first (preset required;
///                                                        its Linear key creates the issue)
///   `{ image, plain: { title, message } }`            — no ticket (preset required if any exist)
/// plus optional `preset` (name; absent/"auto" = label auto-select in ticket mode) /
/// `claudeAccount` / `agentInstructions` / `claudeInstructions`. `image` is a clone-source
/// image reference (`rmng/template:<name>`) from `GET /api/images`.
async fn clone(
    State(app): State<App>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let str_field = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::to_string);

    let image = str_field("image").filter(|s| !s.is_empty()).ok_or_else(|| bad("body must include { image }".into()))?;
    let claude_account = str_field("claudeAccount");
    let agent_instructions = str_field("agentInstructions");
    let claude_instructions = str_field("claudeInstructions");
    let cfg = app.config();
    let prefix = cfg.docker.hostname_prefix.clone();

    // An explicitly chosen preset (by name); absent/"auto" means auto-select in
    // ticket mode and "required, so error" in plain/create mode (checked per mode).
    let explicit = match str_field("preset").map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "auto") {
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
        let display = if suffix.is_empty() { title.to_string() } else { format!("{title} ({suffix})") };
        (hostname, display)
    };

    // Plain (no-ticket) clone: a preset must be picked whenever any are configured.
    if let Some(plain) = body.get("plain").filter(|v| v.is_object()) {
        let title = plain.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let message = plain.get("message").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if title.is_empty() {
            return Err(bad("plain.title is required".into()));
        }
        let env = match explicit {
            Some(p) => preset_env(p),
            None if cfg.presets.is_empty() => Vec::new(),
            None => return Err(bad(format!("a preset is required (configured: {})", preset_names(&cfg)))),
        };
        let (hostname, display) = derive(&app, &linear::plain_hostname_base(&prefix, &title), &title);
        let spec = CloneSpec {
            source_image: image,
            new_hostname: hostname,
            linear: Some(LinearMeta { display_name: Some(display), ..Default::default() }),
            claude_account,
            first_message: Some(message).filter(|m| !m.is_empty()),
            agent_instructions,
            claude_instructions,
            env,
        };
        let op = jobs::start_clone(&app, spec).map_err(|e| bad(e.to_string()))?;
        return Ok(Json(json!({ "ok": true, "op": op })));
    }

    // Ticket / create mode. `op_key` is the API key proven to reach the issue (used
    // for the state mutation); the preset drives the clone's env + LINEAR_API_KEY.
    let (issue, op_key, preset) = resolve_issue(&app, &cfg, explicit, &body).await.map_err(bad)?;
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
        claude_account,
        first_message: None,
        agent_instructions,
        claude_instructions,
        env: preset_env(&preset),
    };
    let op = jobs::start_clone(&app, spec).map_err(|e| bad(e.to_string()))?;
    Ok(Json(json!({ "ok": true, "op": op })))
}

/// The preset's env plus its Linear key as `LINEAR_API_KEY` (auths the clone's
/// `linear` MCP). A `LINEAR_API_KEY` var set explicitly in the preset wins.
fn preset_env(p: &wire::Preset) -> Vec<wire::EnvVar> {
    let mut vars = p.vars.clone();
    if !p.linear_key.is_empty() && !vars.iter().any(|v| v.key == "LINEAR_API_KEY") {
        vars.push(wire::EnvVar { key: "LINEAR_API_KEY".into(), value: p.linear_key.clone() });
    }
    vars
}

fn preset_names(cfg: &wire::AppConfig) -> String {
    cfg.presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
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
        let title = create.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
        let description = create.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let Some(preset) = explicit else {
            return Err("creating a ticket requires a preset (its Linear key creates the issue)".into());
        };
        if preset.linear_key.is_empty() {
            return Err(format!("preset '{}' has no Linear API key — required to create a ticket", preset.name));
        }
        let prefix = team.trim().to_ascii_lowercase();
        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("create.team must be a Linear team key like \"we\"".into());
        }
        if title.is_empty() {
            return Err("create.title is required".into());
        }
        let issue = linear::create_issue(&app.http, &preset.linear_key, &prefix, title, description)
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
    let (issue, op_key) =
        linear::fetch_issue_any(&app.http, &keys, &r).await.map_err(|e| e.to_string())?;
    let preset = match explicit {
        Some(p) => p.clone(),
        None => linear::pick_preset_by_labels(&cfg.presets, &issue.labels).cloned().ok_or_else(|| {
            let labels = if issue.labels.is_empty() { "(none)".into() } else { issue.labels.join(", ") };
            format!(
                "no preset matches ticket {}'s labels [{labels}] — pick a preset explicitly (configured: {})",
                issue.identifier,
                preset_names(cfg),
            )
        })?,
    };
    Ok((issue, op_key, preset))
}

// --- images (clone-source templates) ---------------------------------------

/// `GET /api/images` — the clone-source images (`rmng.image=1`), each with the host ids of
/// live clones currently running on it (`in_use_by`). The Docker layer leaves `in_use_by`
/// empty (it's control-plane state); we fill it here from `state.json`: any host whose
/// `source` equals the image reference. A daemon error surfaces as 502.
async fn images_list(State(app): State<App>) -> Result<Json<Vec<wire::ImageInfo>>, (StatusCode, String)> {
    let mut images = app
        .docker
        .list_rmng_images()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    fill_in_use_by(&mut images, &app.store.get().hosts);
    Ok(Json(images))
}

/// Fill each image's `in_use_by` with the ids of hosts whose `source` equals the image
/// reference (the Docker layer leaves it empty — it's control-plane state). Pure over
/// (images, hosts) so it's unit-testable independent of the daemon.
fn fill_in_use_by(images: &mut [wire::ImageInfo], hosts: &[wire::Host]) {
    for img in images.iter_mut() {
        img.in_use_by = hosts
            .iter()
            .filter(|h| h.source.as_deref() == Some(img.reference.as_str()))
            .map(|h| h.id.clone())
            .collect();
    }
}

#[derive(Deserialize)]
struct BootstrapReq {
    /// DNS-label image name → `rmng/template:<name>`.
    name: String,
}

/// `POST /api/images/bootstrap` — build the wizard base image `rmng/template:<name>` from
/// the fixed base OS (from-zero). Returns the driving Operation (kind `bootstrap`, which the
/// wizard watches for).
async fn images_bootstrap(
    State(app): State<App>,
    Json(req): Json<BootstrapReq>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    jobs::start_bootstrap(&app, &req.name)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
struct CommitReq {
    /// Host id of the managed clone to commit.
    host: String,
    /// DNS-label image name → `rmng/template:<name>`.
    name: String,
}

/// `POST /api/images/commit` — commit a running clone to a new clone-source image
/// `rmng/template:<name>`. Returns the driving Operation (kind `commit`).
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
/// still referenced: any host's `source` equals it, OR a running op (clone/commit) uses it.
/// Otherwise the daemon's own "in use by a container" 409 is surfaced as 409 too.
async fn images_delete(
    State(app): State<App>,
    Json(req): Json<ImageDeleteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let reference = req.reference.trim();
    if reference.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "reference is required".into()));
    }
    let st = app.store.get();
    let users: Vec<String> = st
        .hosts
        .iter()
        .filter(|h| h.source.as_deref() == Some(reference))
        .map(|h| h.id.clone())
        .collect();
    if !users.is_empty() {
        return Err((
            StatusCode::CONFLICT,
            format!("image is in use by {} clone(s): {}", users.len(), users.join(", ")),
        ));
    }
    // A running clone-from-this-image or commit-to-this-reference also blocks removal.
    let busy = st.operations.iter().any(|o| {
        o.status == wire::OperationStatus::Running
            && (o.source.as_deref() == Some(reference) || o.target == reference)
    });
    if busy {
        return Err((StatusCode::CONFLICT, "image is in use by a running operation".into()));
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
    let container = host
        .container
        .as_deref()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("'{}' has no container to redeploy", req.id)))?;
    crate::provision::redeploy_clone(&app, container, req.daemon_only, |step, msg| {
        tracing::info!("redeploy {} ({container}) {step}: {msg}", req.id);
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /api/monitors/apply` — apply the saved monitor layout to every running clone
/// (rewrites its `RMNG_MONITORS` + restarts its GNOME session + daemon). Restarts the
/// clones' desktops, so it's an explicit button rather than part of Save.
async fn monitors_apply(State(app): State<App>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let clones: Vec<(String, String)> = app
        .store
        .get()
        .hosts
        .iter()
        .filter_map(|h| h.container.clone().map(|c| (h.id.clone(), c)))
        .collect();
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for (id, container) in clones {
        match crate::provision::apply_monitors(&app, &container, |step, msg| {
            tracing::info!("apply-monitors {id} ({container}) {step}: {msg}");
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
    // A wizard-finish flip (`setupComplete` false → true) is where the lazy `rmng` network is
    // first materialized. Do it here so a clone create later doesn't have to; a failure is
    // NON-fatal (the config is already saved) — surface it as `networkWarning` in the response
    // so the wizard can show it (the network also gets created on the first clone).
    let mut network_warning: Option<String> = None;
    if !old.setup_complete && merged.setup_complete {
        if let Err(e) = app.docker.ensure_network().await {
            tracing::warn!("ensure_network at wizard finish failed: {e}");
            network_warning = Some(e.to_string());
        }
    }
    *app.cfg.write().unwrap() = merged.clone();
    let resp = ConfigPutResponse { restart_required, config: merged.redacted() };
    let mut body = serde_json::to_value(&resp).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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

// --- Claude accounts -------------------------------------------------------

/// An error body the frontend's `postJson` reads as `{ error }` (vs. a bare string).
fn err_json(code: StatusCode, msg: impl ToString) -> (StatusCode, Json<serde_json::Value>) {
    (code, Json(json!({ "error": msg.to_string() })))
}

type JsonResult = Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)>;

#[derive(Deserialize)]
struct ImportCheckReq {
    host: String,
}

/// `POST /api/claude/import/check` — confirm a clone is signed in to Claude Code via
/// claude.ai and report the account identity (so the UI can show it before the
/// operator mints + pastes a long-lived token).
async fn claude_import_check(State(app): State<App>, Json(req): Json<ImportCheckReq>) -> JsonResult {
    let host = host_by_id(&app, &req.host)
        .ok_or_else(|| err_json(StatusCode::BAD_REQUEST, format!("unknown host '{}'", req.host)))?;
    let st = crate::claude::check_clone_auth(&app, &host)
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({
        "ok": true,
        "email": st.email,
        "orgName": st.org_name,
        "subscriptionType": st.subscription_type,
    })))
}

#[derive(Deserialize)]
struct ImportReq {
    host: String,
}

/// `POST /api/claude/import` — import a Claude account from a signed-in clone: store
/// the clone's OAuth pair (the server owns its refresh lifecycle from here on), then
/// clear the clone's credentials file. Kicks an immediate usage poll so it shows at once.
async fn claude_import(State(app): State<App>, Json(req): Json<ImportReq>) -> JsonResult {
    let host = host_by_id(&app, &req.host)
        .ok_or_else(|| err_json(StatusCode::BAD_REQUEST, format!("unknown host '{}'", req.host)))?;
    let res = crate::claude::import_clone_account(&app, &host)
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e))?;
    let _ = crate::claude::poll_once(&app).await;
    Ok(Json(json!({ "ok": true, "email": res.email, "cleared": res.cleared })))
}

/// `POST /api/claude/refresh` — force one usage poll now.
async fn claude_refresh(State(app): State<App>) -> Json<serde_json::Value> {
    let any429 = crate::claude::poll_once(&app).await.unwrap_or(false);
    Json(json!({ "ok": true, "rateLimited": any429 }))
}

/// `GET /api/claude/recommended` — the account the clone dialog should pre-select.
async fn claude_recommended(State(app): State<App>) -> Json<serde_json::Value> {
    Json(json!({ "email": crate::claude::recommend(&app) }))
}

#[derive(Deserialize)]
struct SwapReq {
    host: String,
    /// Account email, `auto`, `none`, or `group:<name>`.
    account: String,
}

/// `POST /api/claude/swap` — change a clone's Claude account/group. `account` is an
/// email, `auto`, `group:<name>`, or `none`. Binding to a group enrolls the clone in
/// rotation; `none` removes the clone's credentials so it runs with no token.
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
    let container = host
        .container
        .clone()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("'{}' has no container", host.id)))?;
    let assignment = crate::claude::resolve_assignment(&app, Some(&req.account))
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "no imported Claude accounts".into()))?;
    let selection = crate::claude::normalize_selection(Some(&req.account));
    let (group, email) = match assignment {
        crate::claude::Assignment::None => {
            crate::claude::clear_clone_token(&app, &container)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            app.claude.forget_pushed(&host.id);
            (None, None)
        }
        crate::claude::Assignment::Group { name, initial } => {
            crate::claude::push_account_to_clone(&app, &host.id, &container, &initial)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            (Some(name), Some(initial))
        }
        crate::claude::Assignment::Account(a) => {
            crate::claude::push_account_to_clone(&app, &host.id, &container, &a)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
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
    Ok(Json(json!({ "ok": true, "account": email, "group": group, "selection": selection })))
}

/// `POST /api/claude/rotate` — run one group-rotation pass immediately (the rotator
/// otherwise runs every 10 min). Useful for ops + testing.
async fn claude_rotate(State(app): State<App>) -> Json<serde_json::Value> {
    crate::claude::rotate_once(&app).await;
    Json(json!({ "ok": true }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{Host, ImageInfo};

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
    fn host_on(id: &str, source: Option<&str>) -> Host {
        Host {
            id: id.into(),
            container: Some("c".into()),
            source: source.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn in_use_by_maps_hosts_by_source_reference() {
        let mut images = vec![image("rmng/template:a"), image("rmng/template:b")];
        let hosts = vec![
            host_on("h1", Some("rmng/template:a")),
            host_on("h2", Some("rmng/template:a")),
            host_on("h3", Some("rmng/template:b")),
            host_on("h4", None),                    // unmanaged/no source → counted nowhere
            host_on("h5", Some("rmng/template:z")), // source not in the image list → ignored
        ];
        fill_in_use_by(&mut images, &hosts);
        assert_eq!(images[0].in_use_by, vec!["h1", "h2"]);
        assert_eq!(images[1].in_use_by, vec!["h3"]);
    }

    #[test]
    fn in_use_by_empty_when_no_hosts_reference_it() {
        let mut images = vec![image("rmng/template:a")];
        let hosts = vec![host_on("h1", Some("rmng/template:other")), host_on("h2", None)];
        fill_in_use_by(&mut images, &hosts);
        assert!(images[0].in_use_by.is_empty());
    }
}
