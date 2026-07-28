//! Port 2 — the web API + SSE + static frontend. Phase 1 + the Phase-2 clone/
//! delete surface; the rest (Linear/Claude/chat/config/…) lands as those modules
//! are ported.

use std::convert::Infallible;
use std::path::Path;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path as AxPath, State},
    http::{HeaderMap, StatusCode, header},
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{get, post, put},
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
        .route("/api/stats", get(stats_get))
        .route("/api/tokens", get(tokens_get))
        .route("/api/activate", post(activate))
        .route("/api/reorder", post(reorder))
        .route("/api/clone", post(clone))
        .route("/api/layout/activate", post(layout_activate))
        .route("/api/delete", post(delete))
        .route("/api/notes/:id", get(notes_get).put(notes_save))
        .route("/api/upload", post(upload))
        .route("/uploads/:file", get(uploads_serve))
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
        .route("/api/chat/:id/schedule", post(chat_schedule))
        .route(
            "/api/chat/:id/schedule/:sid",
            axum::routing::delete(chat_schedule_cancel),
        )
        .route("/api/hosts/:id/forwards", put(forwards_put))
        .route("/api/hosts/:id/group", post(clone_group))
        .route("/api/hosts/:id/archive", post(archive))
        .route("/api/hosts/:id/unarchive", post(unarchive))
        .route("/api/hosts/:id/mcp", post(clone_mcp))
        .route("/api/hosts/:id/exec", post(clone_exec))
        // Group-proxy onboarding + CRUD (thin proxies to each group instance's management API).
        .route("/api/groups", post(groups_create))
        .route("/api/groups/:name", axum::routing::delete(groups_delete))
        .route(
            "/api/groups/:name/accounts/login/start",
            post(group_login_start),
        )
        .route(
            "/api/groups/:name/accounts/login/status",
            get(group_login_status),
        )
        .route(
            "/api/groups/:name/accounts/login/complete",
            post(group_login_complete),
        )
        .route(
            "/api/groups/:name/accounts/delete",
            post(group_account_delete),
        )
        .route("/api/usage/refresh", post(usage_refresh))
        // The `rmng-cliproxy` sidecar's status + the operator's deliberate roll-forward. The
        // `/cc` router itself no longer lives here — it moved into that container so a
        // control-server update can't interrupt in-flight agent work (see `groupproxy.rs`).
        .route("/api/groupproxy", get(groupproxy_get))
        .route("/api/groupproxy/restart", post(groupproxy_restart))
        // Internal, admin-secret-authenticated: the out-of-process `/cc` proxy's token-delta
        // intake. Registered BEFORE the SPA fallback below.
        .route("/internal/tokens", post(internal_tokens))
        // Tombstone for the `/cc` router that used to live here. Without it these paths reach
        // the SPA fallback and an agent POST gets `200 text/html` (or a bare 405) — an opaque
        // parse error rather than a diagnosis. See [`cc_moved`].
        .route("/cc", axum::routing::any(cc_moved))
        .route("/cc/*rest", axum::routing::any(cc_moved));

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
    // clone screenshots and note uploads. LAN-only service; JSON routes are unaffected in
    // practice.
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

/// `GET /events` — five multiplexed streams on one connection:
///   - the persisted `ControlState` as the default (unnamed) event → the client's
///     `onmessage`: full snapshot on connect, then one frame per change;
///   - the volatile per-clone CPU/RAM map as a named `stats` event → the client's
///     `addEventListener("stats")`: latest snapshot on connect, then one per poll tick;
///   - CT-wide CPU/RAM/disk as a named `lxcStats` event;
///   - the volatile port-forward runtime map as a named `forwards` event;
///   - safe per-clone newly-processed token totals as a named `tokens` event.
///
/// Stats, LXC stats, forwards, and tokens ride separate SSE-only buses
/// ([`crate::monitor::StatsBus`], [`crate::monitor::LxcStatsBus`],
/// [`crate::forward::ForwardBus`], [`crate::tokens::TokenBus`]) so they never enter `ControlState` / `state.json`
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

    let (lxc_snapshot, lxc_rx) = app.lxc_stats.subscribe();
    let lxc_initial =
        futures::stream::once(
            async move { Ok(Event::default().event("lxcStats").data(lxc_snapshot)) },
        );
    let lxc_updates = BroadcastStream::new(lxc_rx).filter_map(|r| async move {
        match r {
            Ok(json) => Some(Ok(Event::default().event("lxcStats").data(json))),
            Err(_) => None,
        }
    });
    let lxc_stream = lxc_initial.chain(lxc_updates);

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

    let (token_snapshot, token_rx) = app.tokens.subscribe();
    let token_initial =
        futures::stream::once(
            async move { Ok(Event::default().event("tokens").data(token_snapshot)) },
        );
    let token_updates = BroadcastStream::new(token_rx).filter_map(|r| async move {
        match r {
            Ok(json) => Some(Ok(Event::default().event("tokens").data(json))),
            Err(_) => None,
        }
    });
    let token_stream = token_initial.chain(token_updates);

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
            futures::stream::select(
                futures::stream::select(
                    futures::stream::select(stats_stream, lxc_stream),
                    fwd_stream,
                ),
                token_stream,
            ),
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

/// `GET /api/stats` — the current per-clone resource-usage snapshot, matching the first named
/// `stats` `/events` frame. Volatile by design: it is never persisted in [`ControlState`].
async fn stats_get(State(app): State<App>) -> Response {
    let (snapshot, _rx) = app.stats.subscribe();
    ([(header::CONTENT_TYPE, "application/json")], snapshot).into_response()
}

/// `GET /api/tokens` — the current safe per-clone cumulative-token snapshot, matching the first
/// named `tokens` `/events` frame. It intentionally contains no activity timestamps or account data.
async fn tokens_get(State(app): State<App>) -> Response {
    let (snapshot, _rx) = app.tokens.subscribe();
    ([(header::CONTENT_TYPE, "application/json")], snapshot).into_response()
}

#[derive(Deserialize)]
struct ActivateReq {
    #[serde(default)]
    id: Option<String>,
}

async fn activate(State(app): State<App>, Json(req): Json<ActivateReq>) -> Json<ControlState> {
    // Stamp "operator looked at this now" for both the clone being left and the one being
    // entered: each has just been on screen. The monitor reads these to suppress a later
    // working→idle notification for a clone whose output the operator already saw (see
    // `monitor::should_flag_unread`). The clone being left is the crucial one — after the
    // operator switches away, that timestamp is what marks its final output as already seen.
    let now = crate::clone_ops::now_ms();
    let previously_selected = app.store.get().selected;
    if let Some(prev) = previously_selected.as_deref() {
        app.views.mark(prev, now);
    }
    if let Some(id) = req.id.as_deref() {
        app.views.mark(id, now);
    }
    Json(app.store.mutate(|s| {
        // Selecting a clone acknowledges its prior working→not-working transition.
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

/// Validate a clone's proposed forward set against the whole state and normalize it into
/// `PortForward`s (ids derived `f{local_port}`). Errors: port 0, duplicate local port
/// within the request, or a local port already claimed by a *different* clone (the viewer
/// binds them all on one machine → the local-port space is global).
fn validate_forwards(
    state: &wire::ControlState,
    host_id: &str,
    inputs: Vec<ForwardInput>,
) -> Result<Vec<wire::PortForward>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    // Local ports claimed by OTHER clones.
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

/// `PUT /api/hosts/:id/forwards` — replace a clone's forward rules. Validated
/// synchronously (returns 400 on conflict); persisted to `state.json`; the media plane
/// re-pushes the new set to the viewer off the store broadcast.
async fn forwards_put(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<ForwardsPutReq>,
) -> Result<Json<ControlState>, (StatusCode, String)> {
    let state = app.store.get();
    if !state.hosts.iter().any(|h| h.id == id) {
        return Err((StatusCode::NOT_FOUND, format!("no clone '{id}'")));
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
async fn clone_mcp(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<wire::McpCallRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let host = clone_by_id(&app, &id).ok_or((StatusCode::NOT_FOUND, format!("no clone '{id}'")))?;
    // Headless clones have no desktop: `gnome-headless.service` and the capture daemon are
    // disabled at create time, so there is no MCP on :9004 to dial. Short-circuit with a clear
    // reason rather than letting `proxy_to_daemon` surface a bare "connection refused" 502 that
    // reads like a transient outage. Checked before `archived`: unarchiving would not give it a
    // desktop, so headlessness is the more actionable message.
    if host.headless {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "clone '{id}' is headless (no desktop) — `rmng desktop` does not apply; \
                 use `rmng exec`/`rmng ssh` or the viewer's terminal instead"
            ),
        ));
    }
    if host.archived {
        return Err((
            StatusCode::CONFLICT,
            format!("clone '{id}' is archived; unarchive it first"),
        ));
    }
    let content = proxy_to_daemon(&app, &host, &req.tool, &req.args)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(content))
}

/// Proxy a desktop/window `tools/call` to a clone's clone-daemon MCP (dialed by container
/// name via Docker DNS — `App::dial_clone`) and return its `result.content`. Moved here from
/// `mcp.rs` when the global MCP was retired; behavior is unchanged.
async fn proxy_to_daemon(
    app: &App,
    host: &wire::RmngClone,
    name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let port = app.config().listen.daemon_mcp;
    let url = format!("http://{}:{port}/", app.dial_clone(host).await);
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

/// The clone's desktop/agent user (uid `1000`, name `rmng`) — the owner of the `systemd --user`
/// graphical session. `rmng exec` seeds its env from that session so GUI apps and the in-clone
/// `claude` CLI just work.
const DESKTOP_UID: &str = "1000";
const DESKTOP_USER: &str = "rmng";

fn is_desktop_user(user: &str) -> bool {
    user == DESKTOP_UID || user == DESKTOP_USER
}

/// Parse `systemctl show-environment` output into `KEY=VAL` env entries, keeping only lines whose
/// key is a valid environment-variable name (skips blanks / any stray non-assignment lines). The
/// value is passed through verbatim — everything after the first `=` — so entries like
/// `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus` survive intact.
fn parse_env_lines(s: &str) -> Vec<String> {
    s.lines()
        .filter(|line| match line.split_once('=') {
            Some((k, _)) => {
                !k.is_empty()
                    && k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            }
            None => false,
        })
        .map(str::to_string)
        .collect()
}

/// Merge caller `overrides` (`KEY=VAL`) over a `base` env, caller-wins: any base entry whose key a
/// caller entry also sets is dropped, then the overrides are appended.
fn merge_env(base: &mut Vec<String>, overrides: &[String]) {
    let keys: std::collections::HashSet<&str> = overrides
        .iter()
        .filter_map(|e| e.split_once('=').map(|(k, _)| k))
        .collect();
    base.retain(|e| e.split_once('=').map(|(k, _)| !keys.contains(k)).unwrap_or(true));
    base.extend(overrides.iter().cloned());
}

/// The clone's live desktop-session environment, read from its `systemd --user` manager
/// (`systemctl --user show-environment`): `WAYLAND_DISPLAY`, `DISPLAY`, `XAUTHORITY`,
/// `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, the session `PATH` (with `~/.local/bin`), plus the
/// agent/control vars the manager imports from `/etc/environment` (via the `environment.d` →
/// `/etc/environment` symlink). A bare `docker exec` inherits none of this — it gets only the
/// container's `Config.Env`, since nothing on that path runs PAM — so `clone_exec` and the
/// `termplane` tmux execs seed the exec env with it for the desktop user.
///
/// This works on a **headless** clone too: the user manager runs there (linger), it just carries no
/// compositor vars (`WAYLAND_DISPLAY`/`DISPLAY`/`XAUTHORITY`). Everything from `/etc/environment` is
/// present either way. Returns `KEY=VAL` entries, or empty (with a debug log) when the user manager
/// isn't reachable yet — a still-booting clone — in which case the exec simply runs without the
/// session env.
pub(crate) async fn desktop_session_env(app: &App, clone_id: &str) -> Vec<String> {
    // `show-environment` talks to the per-user bus, which needs XDG_RUNTIME_DIR; the agent user's
    // runtime dir is the fixed `/run/user/<uid>`.
    let cmd = [
        "systemctl".to_string(),
        "--user".to_string(),
        "show-environment".to_string(),
    ];
    let runtime = format!("XDG_RUNTIME_DIR=/run/user/{DESKTOP_UID}");
    match app
        .docker
        .exec_capture(clone_id, &cmd, DESKTOP_UID, None, &[runtime], None)
        .await
    {
        Ok(r) if r.exit_code == 0 => parse_env_lines(&r.stdout),
        Ok(r) => {
            tracing::debug!(
                clone = clone_id,
                code = r.exit_code,
                "show-environment unavailable: {}",
                r.stderr.trim()
            );
            Vec::new()
        }
        Err(e) => {
            tracing::debug!(clone = clone_id, "show-environment exec failed: {e}");
            Vec::new()
        }
    }
}

/// `POST /api/hosts/:id/exec` — run a single non-interactive command inside the clone via
/// docker exec (`rmng exec`). Body is [`wire::ExecRequest`]; returns [`wire::ExecResult`]
/// (exit code + captured stdout/stderr). Empty argv → 400; unknown clone → 404; a bad
/// stdin payload → 400; a daemon/exec failure (e.g. container not running) → 502. Defaults
/// the run-as user to uid `1000` (the clone's agent user) when unset.
async fn clone_exec(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<wire::ExecRequest>,
) -> Result<Json<wire::ExecResult>, (StatusCode, String)> {
    if req.cmd.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "cmd must not be empty".into()));
    }
    let host = clone_by_id(&app, &id).ok_or((StatusCode::NOT_FOUND, format!("no clone '{id}'")))?;
    if host.archived {
        return Err((
            StatusCode::CONFLICT,
            format!("clone '{id}' is archived; unarchive it first"),
        ));
    }
    let stdin = match &req.stdin_b64 {
        Some(b64) => Some(
            B64.decode(b64)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid stdinB64: {e}")))?,
        ),
        None => None,
    };
    let user = req.user.clone().unwrap_or_else(|| DESKTOP_UID.to_string());
    // For the desktop agent user, seed the exec env from the clone's live `systemd --user` session
    // so GUI apps and the in-clone `claude` CLI just work (WAYLAND_DISPLAY, DISPLAY, XDG_RUNTIME_DIR,
    // DBUS, the session PATH, agent vars) — a bare docker exec inherits none of it. The caller's
    // explicit `env` always wins on a key clash. Other run-as users get only what they pass (their
    // session, if any, is not the desktop one).
    let env = if is_desktop_user(&user) {
        let mut base = desktop_session_env(&app, &host.id).await;
        merge_env(&mut base, &req.env);
        base
    } else {
        req.env.clone()
    };
    // Fire-and-forget: launch detached and return at once (no capture, no wait). Any `stdin_b64` is
    // ignored — there is nothing attached to feed it to. Exit code is reported as 0 = "spawned".
    if req.detach {
        app.docker
            .exec_detached(&host.id, &req.cmd, &user, req.workdir.as_deref(), &env)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
        return Ok(Json(wire::ExecResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }));
    }
    let result = app
        .docker
        .exec_capture(
            &host.id,
            &req.cmd,
            &user,
            req.workdir.as_deref(),
            &env,
            stdin.as_deref(),
        )
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(result))
}

/// Resolve the parent clone for a fleet-CLI clone create (the sub-clone relationship).
/// Precedence: a `topLevel` body flag → `None`; an explicit `parent` body id → validated as a
/// top-level managed clone; otherwise auto-detect the calling clone from its per-clone router
/// key header (`X-RMNG-Proxy-Key`, the same bearer the `/cc` proxy trusts, mapped by
/// [`crate::cliproxy::InstanceManager::clone_for_token`]) and nest under it only when the caller
/// is itself top-level — nesting is one level deep, so a request from a sub clone (or from
/// outside the fleet with no key) yields a top-level clone. `topLevel` + `parent` is an error.
fn resolve_parent(
    app: &App,
    body: &serde_json::Value,
    headers: &HeaderMap,
) -> Result<Option<String>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let top_level = body
        .get("topLevel")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let explicit = body
        .get("parent")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if top_level && explicit.is_some() {
        return Err(bad("`topLevel` and `parent` are mutually exclusive".into()));
    }
    if top_level {
        return Ok(None);
    }
    let st = app.store.get();
    let top_level_managed =
        |id: &str| st.hosts.iter().any(|h| h.id == id && h.managed && h.parent.is_none());
    if let Some(pid) = explicit {
        return match st.hosts.iter().find(|h| h.id == pid) {
            None => Err(bad(format!("parent clone '{pid}' not found"))),
            Some(h) if !h.managed => {
                Err(bad(format!("parent clone '{pid}' is not a managed clone")))
            }
            Some(h) if h.parent.is_some() => Err(bad(format!(
                "parent clone '{pid}' is itself a sub clone; sub clones are one level deep"
            ))),
            Some(_) => Ok(Some(pid.to_string())),
        };
    }
    // Auto-detect: the calling clone proves its identity with its own per-clone router key.
    let caller = headers
        .get("x-rmng-proxy-key")
        .and_then(|v| v.to_str().ok())
        .and_then(|key| app.cliproxy.clone_for_token(key));
    Ok(caller.filter(|id| top_level_managed(id)))
}

/// The effective account group + preset for a fleet-CLI clone, applying sub-clone inheritance:
/// a sub clone inherits its `parent`'s group / preset unless the request specified one (an
/// explicit `--group`/`--preset`, including `none`, counts as specified and overrides). No
/// parent, or a parent with no group/preset, yields `None` (same as a plain top-level clone).
/// Pure — unit-tested. The returned preset borrows `presets` (the live config preset list).
fn effective_group_preset<'a>(
    parent: Option<&wire::RmngClone>,
    group_specified: bool,
    resolved_group: Option<String>,
    preset_specified: bool,
    explicit: Option<&'a wire::Preset>,
    presets: &'a [wire::Preset],
) -> (Option<String>, Option<&'a wire::Preset>) {
    let group = if group_specified {
        resolved_group
    } else {
        // A blank parent group can't happen after `normalize_clone_groups`, but filter anyway
        // rather than inheriting an empty name past `resolve_clone_group`'s fallback chain.
        parent.map(|h| h.group.clone()).filter(|g| !g.is_empty())
    };
    let preset = if preset_specified {
        explicit
    } else {
        parent
            .and_then(|h| h.preset_name.as_deref())
            .and_then(|name| presets.iter().find(|p| p.name == name))
    };
    (group, preset)
}

/// A preset's default account group. `config::normalize_groups` keeps every preset pointed at
/// a real group, but this re-checks against the live config anyway: a config written by an
/// older build (or by hand) can still carry a blank or dangling name, and binding a clone
/// to a group with no CLIProxyAPI instance would break its inference silently.
fn preset_default_group(cfg: &wire::AppConfig, preset: Option<&wire::Preset>) -> Option<String> {
    let name = preset?.group.trim();
    cfg.groups
        .iter()
        .find(|g| g.name == name)
        .map(|g| g.name.clone())
}

/// The account group a new clone binds, resolving the full precedence chain. Every clone binds
/// one, so this returns a concrete name; the chain is, strongest first:
///
/// 1. an explicit `group` on the request (already validated by `resolve_group`),
/// 2. a sub clone's inherited parent group (arrives as `group` with `group_specified` false),
/// 3. the effective preset's default group,
/// 4. the first configured group — the backstop that makes the invariant total.
///
/// Step 4 can only be reached with no explicit group, no parent, and a preset with no (or a
/// dangling) group, which `config::normalize_groups` should have already fixed. Pure —
/// unit-tested; the `clone` handler applies it per mode because each resolves its preset
/// differently.
fn resolve_clone_group(
    cfg: &wire::AppConfig,
    group: Option<String>,
    preset: Option<&wire::Preset>,
) -> Result<String, (StatusCode, String)> {
    group
        .or_else(|| preset_default_group(cfg, preset))
        .or_else(|| cfg.groups.first().map(|g| g.name.clone()))
        .ok_or((
            // Unreachable in practice — `config::normalize_groups` seeds a group at load and
            // after every save. An error beats silently creating an inference-less clone.
            StatusCode::INTERNAL_SERVER_ERROR,
            "no account group is configured — add one in Settings".to_string(),
        ))
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
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let str_field = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::to_string);

    let image = str_field("image")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("body must include { image }".into()))?;
    let requested_group = str_field("group");
    let group = resolve_group(&app, requested_group.as_deref())?;
    let agent_instructions = str_field("agentInstructions");
    let claude_instructions = str_field("claudeInstructions");
    // Cross-cutting like `group`/`preset`: a headless clone (no desktop) in any create mode.
    let headless = body.get("headless").and_then(|v| v.as_bool()).unwrap_or(false);
    let cfg = app.config();
    let prefix = cfg.docker.hostname_prefix.clone();

    // Whether the request carried a `preset` field at all (present ⇒ don't inherit the parent's
    // preset on a sub clone). `auto`/`none`/empty resolve to "no explicit preset".
    let preset_field = str_field("preset").map(|s| s.trim().to_string());
    let preset_specified = preset_field.as_ref().is_some_and(|s| !s.is_empty());
    // An explicitly chosen preset (by name); absent/"auto"/"none" means auto-select in
    // ticket mode and "required, so error" in plain/create mode (checked per mode).
    let explicit = match preset_field
        .filter(|s| !s.is_empty() && s != "auto" && !s.eq_ignore_ascii_case("none"))
    {
        Some(name) => Some(
            cfg.presets
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| bad(format!("unknown preset '{name}'")))?,
        ),
        None => None,
    };

    // Applied per mode because each resolves its preset differently (ticket mode's is
    // label-auto-selected inside `resolve_issue`, so it isn't known until then).
    let clone_group =
        |group: Option<String>, preset: Option<&wire::Preset>| resolve_clone_group(&cfg, group, preset);

    // Sub-clone resolution, shared by every mode: a `topLevel` flag forces top-level, an
    // explicit `parent` id is validated as a top-level managed clone, and otherwise the caller
    // clone is auto-detected from its per-clone router key. The web dialog's "sub clone of X"
    // checkbox sends `parent`, so this is NOT fleet-CLI-only — resolving it here rather than
    // inside the hostname branch is what makes that checkbox work in the UI create modes.
    let parent = resolve_parent(&app, &body, &headers)?;

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
        // A sub clone inherits its parent clone's group + preset BY DEFAULT — a clone created
        // from inside a clone (the common case) should join the same account pool and env as its
        // parent unless the caller overrides it (`--group <name|none>` / `--preset <name|none>`).
        let parent_clone = parent
            .as_deref()
            .and_then(|pid| app.store.get().hosts.into_iter().find(|h| h.id == pid));
        let (eff_group, eff_preset) = effective_group_preset(
            parent_clone.as_ref(),
            requested_group.is_some(),
            group.clone(),
            preset_specified,
            explicit,
            &cfg.presets,
        );
        let spec = CloneSpec {
            source_image: image,
            new_hostname: hostname,
            linear: None,
            // Inherited parent group first, then the effective preset's default.
            group: clone_group(eff_group, eff_preset)?,
            first_message: None,
            agent_instructions,
            claude_instructions,
            preset_name: eff_preset.map(|p| p.name.clone()),
            env: eff_preset
                .map(crate::provision::preset_env_vars)
                .unwrap_or_default(),
            agent_playbook: compose_playbook(&cfg, eff_preset),
            global_prompt: compose_global_prompt(&cfg, eff_preset),
            headless,
            parent,
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
            group: clone_group(group.clone(), explicit)?,
            first_message: Some(message).filter(|m| !m.is_empty()),
            agent_instructions,
            claude_instructions,
            preset_name: explicit.map(|p| p.name.clone()),
            env,
            agent_playbook: compose_playbook(&cfg, explicit),
            global_prompt: compose_global_prompt(&cfg, explicit),
            headless,
            parent: parent.clone(),
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
        // Ticket mode's preset may have been label-auto-selected, so its default group is
        // only knowable here, after `resolve_issue`.
        group: clone_group(group, Some(&preset))?,
        first_message: None,
        agent_instructions,
        claude_instructions,
        preset_name: Some(preset.name.clone()),
        env: crate::provision::preset_env_vars(&preset),
        agent_playbook: compose_playbook(&cfg, Some(&preset)),
        global_prompt: compose_global_prompt(&cfg, Some(&preset)),
        headless,
        parent,
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

/// The effective GLOBAL AGENT PROMPT for a clone (layers **a + c**): the global `globalPrompt`
/// plus the preset's optional `globalPrompt` append (after a blank line). This is the shared
/// operating-memory body written to EVERY agent's native rules file (CLAUDE.md / AGENTS.md).
/// Same shape as [`compose_playbook`] (which yields the node-agent-only b+d append).
pub(crate) fn compose_global_prompt(cfg: &wire::AppConfig, preset: Option<&wire::Preset>) -> String {
    let base = cfg.global_prompt.trim();
    match preset
        .map(|p| p.global_prompt.trim())
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
        // Validate the team key BEFORE resolving the preset from it, so garbage input reads as
        // "that isn't a team key" rather than "no preset claims it".
        let prefix = team.trim().to_ascii_lowercase();
        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("create.team must be a Linear team key like \"we\"".into());
        }
        if title.is_empty() {
            return Err("create.title is required".into());
        }
        // The team key IS the preset choice: it comes from the presets' own ticket-id
        // prefixes, so an omitted `preset` resolves through the same `pick_preset_by_prefix`
        // the existing-ticket mode uses. The web dialog sends the name it resolved; the CLI's
        // `clone create-with-new-ticket` sends only `--team` and relies on this.
        let preset = match explicit {
            Some(p) => p,
            None => linear::pick_preset_by_prefix(&cfg.presets, &prefix).ok_or_else(|| {
                format!(
                    "no preset claims team {} — add it to a preset's ticket-id prefixes, \
                     or name a preset explicitly (configured: {})",
                    prefix.to_uppercase(),
                    preset_names(cfg),
                )
            })?,
        };
        if preset.linear_key.is_empty() {
            return Err(format!(
                "preset '{}' has no Linear API key — required to create a ticket",
                preset.name
            ));
        }
        // The description arrives as markdown from the dialog's rich-text editor, so any
        // pasted image points at this server's LAN-only `/uploads`. Re-host those in Linear
        // first — otherwise the ticket renders broken images for anyone off the network.
        let description = linear::rehost_markdown_images(
            &app.http,
            &preset.linear_key,
            &app.config().data_dir,
            description,
        )
        .await;
        let issue =
            linear::create_issue(&app.http, &preset.linear_key, &prefix, title, &description)
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
/// the managed containers created from it (`in_use_by`; container name == clone id for
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
    /// Clone id of the managed clone to commit.
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

/// `POST /api/hosts/:id/archive` — gracefully stop a managed clone but retain its data.
async fn archive(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    jobs::start_archive(&app, &id)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// `POST /api/hosts/:id/unarchive` — restart a retained archived clone.
async fn unarchive(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    jobs::start_unarchive(&app, &id)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// `POST /api/delete` — destroy a managed CT (or unregister a plain clone).
async fn delete(
    State(app): State<App>,
    Json(req): Json<DeleteReq>,
) -> Result<Json<Operation>, (StatusCode, String)> {
    // Cascade: a sub clone is torn down with its parent. Delete each child first — as its own
    // full delete op (container + volumes + token/router-key/notes teardown) — best-effort, so
    // a child that is momentarily busy doesn't block the parent's removal (the frontend renders
    // a child whose parent has vanished as top-level). One level deep ⇒ no recursion.
    let children: Vec<String> = app
        .store
        .get()
        .hosts
        .iter()
        .filter(|h| h.parent.as_deref() == Some(req.id.as_str()))
        .map(|h| h.id.clone())
        .collect();
    for child in &children {
        app.cliproxy.forget_clone(child);
        if let Err(e) = jobs::start_delete(&app, child) {
            tracing::warn!(target: "clone", "cascade delete of sub clone '{child}' skipped: {e}");
        }
    }
    // Drop the clone's group-proxy router key so a stale bearer can never route again.
    app.cliproxy.forget_clone(&req.id);
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

async fn notes_get(State(app): State<App>, AxPath(id): AxPath<String>) -> Json<serde_json::Value> {
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

/// Repoint every clone whose group is blank or dangling at the first configured group.
/// Wraps [`crate::state::normalize_groups`] with the current config's group list, and skips
/// the `mutate` (a disk write + an SSE broadcast to every browser) when nothing changed —
/// this runs on every reconciler pass, where the steady state is "no change".
///
/// Relies on `config::normalize_groups` having guaranteed at least one group; with none, it
/// is a no-op rather than repointing every clone at a blank name.
pub(crate) fn normalize_clone_groups(app: &App) {
    let groups: Vec<String> = app.config().groups.iter().map(|g| g.name.clone()).collect();
    let Some(fallback) = groups.first().cloned() else {
        return;
    };
    let needs_fix = app
        .store
        .get()
        .hosts
        .iter()
        .any(|h| !groups.iter().any(|g| g == &h.group));
    if !needs_fix {
        return;
    }
    app.store.mutate(|s| {
        crate::state::normalize_groups(s, &fallback, &groups);
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
    /// The account pool this clone's agents route through. Required — a clone always binds
    /// a group.
    #[serde(default)]
    group: Option<String>,
}

/// Resolve a requested group name against the configured groups. `None`/blank means "not
/// specified" (the caller decides what to default to); a name that isn't configured is an
/// error. Unlike earlier revisions there is no `"none"` escape hatch — every clone binds a
/// group, so "bind nothing" is no longer expressible.
fn resolve_group(app: &App, group: Option<&str>) -> Result<Option<String>, (StatusCode, String)> {
    match group.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) if app.config().groups.iter().any(|group| group.name == name) => {
            Ok(Some(name.to_string()))
        }
        Some(name) => Err((
            StatusCode::BAD_REQUEST,
            format!("unknown group '{name}' (configured: {})", group_names(&app.config())),
        )),
        None => Ok(None),
    }
}

/// The configured group names, for error messages.
fn group_names(cfg: &wire::AppConfig) -> String {
    cfg.groups
        .iter()
        .map(|g| g.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `POST /api/hosts/:id/group` — bind a clone to an account group. This is the sole account
/// selection under the group-proxy model: the `/cc` router maps the clone → its group → that
/// group's CLIProxyAPI instance, which owns intra-group account selection + OAuth refresh. No
/// clone-side change is needed — a group swap is a pure map update. Unknown clone → 400;
/// unmanaged row → 400; an unknown or missing group name → 400 (the binding is mandatory, so
/// there is no way to clear it).
async fn clone_group(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<HostGroupReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let host =
        clone_by_id(&app, &id).ok_or((StatusCode::BAD_REQUEST, format!("unknown clone '{id}'")))?;
    if !host.managed {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("'{id}' is not a managed clone"),
        ));
    }
    let group = resolve_group(&app, req.group.as_deref())?.ok_or((
        StatusCode::BAD_REQUEST,
        format!(
            "a group name is required — every clone binds one (configured: {})",
            group_names(&app.config())
        ),
    ))?;
    let group_set = group.clone();
    app.store.mutate(|s| {
        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
            h.group = group_set;
        }
    });
    Ok(Json(json!({ "ok": true, "group": group })))
}

// --- per-clone chat ---------------------------------------------------------

fn clone_by_id(app: &App, id: &str) -> Option<wire::RmngClone> {
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
    let host = clone_by_id(&app, &id)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("unknown clone '{id}'")))?;
    if host.archived {
        return Err((
            StatusCode::CONFLICT,
            format!("clone '{id}' is archived; unarchive it first"),
        ));
    }
    crate::chat::send_chat(&app, &host, &req.text).map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/chat/:id/events` — per-clone chat SSE (snapshot + on change).
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
async fn chat_abort(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Some(host) = clone_by_id(&app, &id) {
        if host.archived {
            return Err((
                StatusCode::CONFLICT,
                format!("clone '{id}' is archived; unarchive it first"),
            ));
        }
        crate::chat::abort_chat(&app, &host).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `ANY /cc` + `/cc/*rest` — the group-proxy router moved to the `rmng-cliproxy` sidecar.
///
/// This exists for one migration window: a clone created before the split holds
/// `ANTHROPIC_BASE_URL=http://rmng-control:9000/cc`, and any agent process ALREADY RUNNING at
/// upgrade time keeps that value until it restarts (PAM reads `/etc/environment` at session
/// start, not continuously). Without this route those requests fall through to the SPA
/// fallback and come back as `200 text/html` on a GET or a bare bodyless `405` on a POST —
/// which surfaces inside the agent as an unintelligible parse/API error.
///
/// So answer in the shape an agent client can actually read: `503` with an
/// Anthropic-style JSON error body naming the new endpoint, plus a `Location` header for
/// anything following redirects manually. 503 rather than 410 is deliberate — it is the one
/// status every agent SDK already treats as retryable, and the condition IS transient: the
/// reconciler restarts the wrapper within ~30 s, after which the retry lands on the sidecar.
async fn cc_moved() -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": format!(
                "the RMNG group proxy moved out of the control-server: use \
                 http://{}:{}/cc instead of this host. This clone's environment is stale; the \
                 reconciler refreshes it and restarts the agent within ~30s — retry.",
                crate::groupproxy::CONTAINER,
                crate::groupproxy::PORT,
            ),
        },
    });
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::LOCATION, crate::groupproxy::cc_base_url()),
            (header::RETRY_AFTER, "30".to_string()),
        ],
        Json(body),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ChatScheduleReq {
    text: String,
    /// Delivery time, epoch milliseconds.
    at: i64,
}

/// `POST /api/chat/:id/schedule` — queue a message for later delivery. The pending queue
/// rides the existing `/events` frame, so there is nothing to poll after this returns.
async fn chat_schedule(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<ChatScheduleReq>,
) -> Result<(StatusCode, Json<wire::ScheduledMessage>), (StatusCode, String)> {
    let host = clone_by_id(&app, &id)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("unknown clone '{id}'")))?;
    // Archived clones reject scheduling for the same reason they reject sending: the queue
    // would just sit there. Being *busy*, by contrast, is fine — that's the point of scheduling.
    if host.archived {
        return Err((
            StatusCode::CONFLICT,
            format!("clone '{id}' is archived; unarchive it first"),
        ));
    }
    let msg = crate::chat::schedule_message(&app, &host.id, &req.text, req.at)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok((StatusCode::CREATED, Json(msg)))
}

/// `DELETE /api/chat/:id/schedule/:sid` — cancel a pending scheduled message.
async fn chat_schedule_cancel(
    State(app): State<App>,
    AxPath((id, sid)): AxPath<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    if !crate::files::is_safe_id(&id) {
        return Err((StatusCode::BAD_REQUEST, format!("invalid clone id '{id}'")));
    }
    if !crate::chat::cancel_schedule(&app, &id, &sid) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no pending scheduled message '{sid}'"),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- group-proxy container: lifecycle + the internal token-delta intake ------

/// `GET /api/groupproxy` — the `rmng-cliproxy` sidecar's status: is it running, on which
/// image/revision, and is that image behind the control-server's own. Feeds the Settings
/// panel's group-proxy section, whose whole purpose is making "the proxy is behind" visible —
/// a control-server update deliberately does NOT roll the sidecar forward (that would kill the
/// in-flight agent turns this split exists to protect), so the operator needs to see the drift.
async fn groupproxy_get(State(app): State<App>) -> Json<wire::GroupProxyStatus> {
    Json(app.docker.group_proxy_status().await)
}

/// `POST /api/groupproxy/restart` — recreate the `rmng-cliproxy` container on the
/// control-server's CURRENT image. This is the operator's deliberate roll-forward: it drops
/// every in-flight agent request in the fleet, which is exactly why it is a button and not a
/// side effect of the control-server update. Returns the sidecar's post-restart status.
async fn groupproxy_restart(
    State(app): State<App>,
) -> Result<Json<wire::GroupProxyStatus>, (StatusCode, String)> {
    // Mint the shared secret first if it's somehow absent, so the recreated container comes up
    // with a working admin channel rather than 401ing the control-server until the next
    // `apply_now`.
    let _ = app.cliproxy.ensure_admin_secret();
    app.docker
        .recreate_group_proxy(&app.config())
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(Json(app.docker.group_proxy_status().await))
}

/// `POST /internal/tokens` — the group-proxy container's token-delta intake, authenticated by
/// the shared admin secret (see [`crate::groupproxy`]). The `/cc` proxy now runs out of process,
/// so its [`crate::tokens::ResponseObserver`] can't touch `clone-tokens.json` directly: two
/// writers would corrupt it. Instead it coalesces increments and POSTs them here in batches,
/// buffering across a control-server restart.
///
/// Each delta carries the clone's token lifecycle epoch, re-validated here by
/// [`crate::tokens::TokenBus`] — a delta from a response that began before an archive/unarchive
/// is dropped exactly as an in-process stale observer was. A bad/missing secret is a 401, which
/// the sender treats as permanent and drops (rather than pinning its buffer forever).
async fn internal_tokens(
    State(app): State<App>,
    headers: HeaderMap,
    Json(deltas): Json<Vec<crate::tokens::TokenDelta>>,
) -> Result<StatusCode, (StatusCode, String)> {
    let presented = headers
        .get(crate::groupproxy::ADMIN_HEADER)
        .and_then(|v| v.to_str().ok());
    if !crate::groupproxy::admin_authorized(app.cliproxy.admin_secret().as_deref(), presented) {
        return Err((StatusCode::UNAUTHORIZED, "admin key required".into()));
    }
    for delta in &deltas {
        app.tokens.apply_remote_delta(&app.store, delta);
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- group-proxy CRUD + onboarding -----------------------------------------

/// Send a group-instance management request through the group-proxy container's admin-forward
/// surface (`/admin/:group/mgmt/*`, [`crate::groupproxy::admin_mgmt_url`]), authenticated with
/// the shared admin secret; the sidecar attaches the instance's own `X-Management-Key` on the
/// far side. The instances bind loopback inside that container, so this indirection is the only
/// route to them now.
///
/// Retries on a *connection* error for up to ~20 s. Two things can be briefly absent: a freshly
/// created group's CLIProxyAPI instance (the supervisor reconciles on a short interval), and the
/// `rmng-cliproxy` container itself right after an operator-triggered roll-forward. Both would
/// otherwise surface the first onboarding call after `POST /api/groups` as a gateway error; this
/// waits them out. Only connect errors are retried — a real HTTP response (even non-2xx) returns
/// immediately.
async fn mgmt_send_retry(
    app: &App,
    method: reqwest::Method,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<reqwest::Response, (StatusCode, String)> {
    let secret = app.cliproxy.ensure_admin_secret();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let mut rb = app
            .http
            .request(method.clone(), url)
            .header(crate::groupproxy::ADMIN_HEADER, &secret);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        match rb.send().await {
            Ok(resp) => return Ok(resp),
            Err(e) if e.is_connect() && std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            }
            Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("group proxy: {e}"))),
        }
    }
}

async fn mgmt_get_json(
    app: &App,
    group: &str,
    path_and_query: &str,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let resp = mgmt_send_retry(
        app,
        reqwest::Method::GET,
        &crate::groupproxy::admin_mgmt_url(group, path_and_query),
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
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("group instance {}: {text}", status.as_u16()),
        ));
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
        // Deleting the last group re-seeds "Default", and any preset that pointed at the
        // deleted group is repointed — a preset must always name a real group.
        config::normalize_groups(&mut cfg);
        config::save(&cfg).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        *app.cfg.write().unwrap() = cfg.clone();
        // Clones bound to the deleted group need the same treatment, and immediately: waiting
        // for the reconciler's next pass would leave them pointed at a group with no instance,
        // so every agent request in them 503s until it runs.
        normalize_clone_groups(&app);
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
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unknown provider '{other}'"),
            ));
        }
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
        &app,
        reqwest::Method::POST,
        &crate::groupproxy::admin_mgmt_url(&name, "/oauth-callback"),
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
    let resp = mgmt_send_retry(
        &app,
        reqwest::Method::DELETE,
        &crate::groupproxy::admin_mgmt_url(
            &name,
            &format!("/auth-files?name={}", urlencode(file)),
        ),
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
    use axum::http::HeaderName;
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
        assert_eq!(
            out,
            json!({ "state": "error", "error": "unknown or expired state" })
        );
    }

    #[test]
    fn login_status_error_without_message_falls_back() {
        let out = normalize_login_status(&json!({ "status": "error" }));
        assert_eq!(
            out,
            json!({ "state": "error", "error": "Authentication failed" })
        );
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
        // Seed the default group, mirroring `config::load` → `config::normalize_groups`. A
        // group-less config is not a reachable production state, and clone creation now
        // resolves a group for every clone, so a bare `Default` here would 500 in tests that
        // are about something else entirely.
        let cfg = wire::AppConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            groups: vec![wire::Group { name: wire::DEFAULT_GROUP.into() }],
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
            s.hosts.push(wire::RmngClone {
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
    async fn clone_group_binds_and_clears_and_validates() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(wire::RmngClone {
                id: "w1".into(),
                host: "w1".into(),
                managed: true,
                ..Default::default()
            });
        });
        *app.cfg.write().unwrap() = wire::AppConfig {
            groups: vec![wire::Group {
                name: "team".into(),
            }],
            ..app.config()
        };

        // Bind to a known group.
        let resp = clone_group(
            State(app.clone()),
            AxPath("w1".into()),
            Json(HostGroupReq {
                group: Some("team".into()),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["group"], "team");
        let host = app
            .store
            .get()
            .hosts
            .into_iter()
            .find(|h| h.id == "w1")
            .unwrap();
        assert_eq!(host.group, "team");

        // The binding can't be cleared — every clone binds a group, so a null/absent name
        // is a 400 and the existing binding is left untouched.
        let err = clone_group(
            State(app.clone()),
            AxPath("w1".into()),
            Json(HostGroupReq { group: None }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("required"), "msg: {}", err.1);
        let host = app
            .store
            .get()
            .hosts
            .into_iter()
            .find(|h| h.id == "w1")
            .unwrap();
        assert_eq!(host.group, "team");

        // An unknown group name is a 400.
        let err = clone_group(
            State(app.clone()),
            AxPath("w1".into()),
            Json(HostGroupReq {
                group: Some("nope".into()),
            }),
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
        // Hostname mode still resolves the account-group binding, so the group must exist.
        *app.cfg.write().unwrap() = wire::AppConfig {
            groups: vec![wire::Group {
                name: "team".into(),
            }],
            ..app.config()
        };
        let body = json!({ "image": "tmpl:latest", "hostname": "w-mod-claude", "group": "team" });
        let resp = clone(State(app.clone()), HeaderMap::new(), Json(body)).await.unwrap().0;
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
        let err = clone(State(app.clone()), HeaderMap::new(), Json(body)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("DNS label"), "msg: {}", err.1);
    }

    #[tokio::test]
    async fn clone_hostname_mode_rejects_unknown_preset() {
        let app = test_app();
        let body = json!({ "image": "tmpl:latest", "hostname": "w1", "preset": "nope" });
        let err = clone(State(app.clone()), HeaderMap::new(), Json(body)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("unknown preset"), "msg: {}", err.1);
    }

    /// Every create mode honours `parent`, not just the fleet-CLI hostname mode.
    ///
    /// Regression: `parent` was resolved inside the hostname branch only, and the plain/ticket
    /// branches hardcoded `parent: None`. The web dialog's "sub clone of X" checkbox sends
    /// `parent` in plain/create mode, so it was silently dropped — the clone came back
    /// top-level with no error. `resolve_parent` had its own passing unit test the whole time,
    /// which is exactly why this slipped through: the bug was in who calls it.
    ///
    /// A source-level assert, deliberately. Every runtime route into `spec.parent` is gated by
    /// a web-layer check identical to `start_clone`'s, so no request body can distinguish
    /// "the branch propagated `parent`" from "the branch dropped it" — a behavioural test here
    /// passes against the bug (verified: reverting both branches to `parent: None` leaves such
    /// a test green). Observing the spec itself would need a Docker image or a test seam that
    /// earns less than it costs, for three call sites in one function.
    ///
    /// So this asserts the property that actually broke: a `CloneSpec` literal in `clone()`
    /// that hardcodes the field, rather than passing the resolved `parent` through.
    #[test]
    fn no_create_mode_hardcodes_parent_none() {
        let src = include_str!("web.rs");
        let handler = src
            .split_once("async fn clone(")
            .expect("clone handler")
            .1
            .split_once("\nfn preset_names(")
            .expect("end of clone handler")
            .0;
        assert!(
            !handler.contains("parent: None"),
            "a CloneSpec in clone() hardcodes `parent: None` — the web dialog's \
             \"sub clone of X\" checkbox sends `parent` in plain/create mode, so that \
             silently drops it and the clone comes back top-level"
        );
        // And the resolution it must use is still shared across the modes, not per-branch.
        assert_eq!(
            handler.matches("resolve_parent(&app").count(),
            1,
            "`parent` should be resolved once for all create modes"
        );
    }

    // --- sub clones: parent resolution + cascade delete ---

    fn push_clone(app: &App, id: &str, managed: bool, parent: Option<&str>) {
        app.store.mutate(|s| {
            s.hosts.push(wire::RmngClone {
                id: id.into(),
                host: id.into(),
                managed,
                parent: parent.map(str::to_string),
                ..Default::default()
            });
        });
    }

    #[tokio::test]
    async fn resolve_parent_explicit_flags_and_validation() {
        let app = test_app();
        push_clone(&app, "p", true, None); // top-level managed clone
        push_clone(&app, "c", true, Some("p")); // its sub clone
        push_clone(&app, "u", false, None); // unmanaged row
        let empty = HeaderMap::new();

        // `topLevel` forces a top-level clone; no hints also → top-level.
        assert_eq!(resolve_parent(&app, &json!({ "topLevel": true }), &empty).unwrap(), None);
        assert_eq!(resolve_parent(&app, &json!({}), &empty).unwrap(), None);
        // A valid explicit top-level parent is accepted.
        assert_eq!(
            resolve_parent(&app, &json!({ "parent": "p" }), &empty).unwrap(),
            Some("p".into())
        );
        // A sub clone, an unmanaged row, and an unknown id are all rejected as parents.
        for pid in ["c", "u", "ghost"] {
            assert!(resolve_parent(&app, &json!({ "parent": pid }), &empty).is_err());
        }
        // `parent` + `topLevel` together is an error.
        assert!(resolve_parent(&app, &json!({ "parent": "p", "topLevel": true }), &empty).is_err());
    }

    #[test]
    fn sub_clone_inherits_group_and_preset_unless_overridden() {
        let presets = vec![
            wire::Preset { name: "parent-preset".into(), ..Default::default() },
            wire::Preset { name: "override-preset".into(), ..Default::default() },
        ];
        let parent = wire::RmngClone {
            id: "p".into(),
            managed: true,
            group: "parent-group".into(),
            preset_name: Some("parent-preset".into()),
            ..Default::default()
        };
        let name = |p: Option<&wire::Preset>| p.map(|p| p.name.clone());

        // Nothing specified → inherit both from the parent.
        let (g, pr) = effective_group_preset(Some(&parent), false, None, false, None, &presets);
        assert_eq!(g, Some("parent-group".into()));
        assert_eq!(name(pr), Some("parent-preset".into()));

        // Explicit group/preset override inheritance.
        let (g, pr) = effective_group_preset(
            Some(&parent), true, Some("other-group".into()), true, Some(&presets[1]), &presets,
        );
        assert_eq!(g, Some("other-group".into()));
        assert_eq!(name(pr), Some("override-preset".into()));

        // Explicit `none` (specified, but resolves to None) opts out of inheritance.
        let (g, pr) = effective_group_preset(Some(&parent), true, None, true, None, &presets);
        assert_eq!(g, None);
        assert_eq!(pr, None);

        // No parent → no inheritance.
        let (g, pr) = effective_group_preset(None, false, None, false, None, &presets);
        assert_eq!(g, None);
        assert!(pr.is_none());

        // Parent names a preset that no longer exists → gracefully no preset.
        let orphan = wire::RmngClone { preset_name: Some("gone".into()), ..parent.clone() };
        let (_g, pr) = effective_group_preset(Some(&orphan), false, None, false, None, &presets);
        assert!(pr.is_none());
    }

    #[test]
    fn preset_default_group_resolves_against_live_groups() {
        let cfg = wire::AppConfig {
            groups: vec![wire::Group { name: "pooled".into() }],
            ..Default::default()
        };
        let bound = wire::Preset { group: "pooled".into(), ..Default::default() };
        assert_eq!(preset_default_group(&cfg, Some(&bound)), Some("pooled".into()));

        // No preset at all → no default.
        assert_eq!(preset_default_group(&cfg, None), None);

        // A blank or dangling group can only come from a hand-edited / older config
        // (`normalize_groups` repoints both) — resolve to None rather than binding the
        // clone to a group that has no CLIProxyAPI instance behind it.
        for g in ["", "deleted"] {
            let p = wire::Preset { group: g.into(), ..Default::default() };
            assert_eq!(preset_default_group(&cfg, Some(&p)), None, "group {g:?}");
        }
    }

    #[test]
    fn clone_group_walks_the_precedence_chain() {
        // Group order matters: "first" is the final backstop, so it is deliberately NOT the
        // preset's group — otherwise the fallback and the preset default are indistinguishable.
        let cfg = wire::AppConfig {
            groups: vec![
                wire::Group { name: "first".into() },
                wire::Group { name: "pooled".into() },
                wire::Group { name: "other".into() },
            ],
            ..Default::default()
        };
        let p = wire::Preset { group: "pooled".into(), ..Default::default() };
        let g = |group: Option<String>, preset: Option<&wire::Preset>| {
            resolve_clone_group(&cfg, group, preset).unwrap()
        };

        // 1. An explicit request group wins over the preset's default.
        assert_eq!(g(Some("other".into()), Some(&p)), "other");
        // 2. A sub clone's inherited parent group likewise (it arrives already resolved).
        assert_eq!(g(Some("other".into()), None), "other");
        // 3. Nothing explicit → the preset's default.
        assert_eq!(g(None, Some(&p)), "pooled");
        // 4. No preset at all → the first configured group.
        assert_eq!(g(None, None), "first");
        // 4. …and likewise for a preset whose group was deleted out from under it.
        let dangling = wire::Preset { group: "deleted".into(), ..Default::default() };
        assert_eq!(g(None, Some(&dangling)), "first");

        // With no groups configured at all the chain has nothing to land on. Unreachable in
        // practice (`config::normalize_groups` seeds one) — assert it errors rather than
        // silently creating a clone that can never reach inference.
        let empty = wire::AppConfig { groups: vec![], ..Default::default() };
        let err = resolve_clone_group(&empty, None, Some(&p)).unwrap_err();
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn resolve_group_validates_against_configured_groups() {
        let app = test_app();
        *app.cfg.write().unwrap() = wire::AppConfig {
            groups: vec![wire::Group { name: "team".into() }],
            ..app.config()
        };
        assert_eq!(resolve_group(&app, Some("team")).unwrap(), Some("team".into()));
        // Absent/blank = "not specified"; the caller decides the fallback.
        assert_eq!(resolve_group(&app, None).unwrap(), None);
        assert_eq!(resolve_group(&app, Some("  ")).unwrap(), None);
        // Unknown names error — and "none" is no longer a sentinel, just an unknown name.
        assert!(resolve_group(&app, Some("ghost")).is_err());
        assert!(resolve_group(&app, Some("none")).is_err());
    }

    #[tokio::test]
    async fn resolve_parent_auto_detects_caller_router_key() {
        let app = test_app();
        push_clone(&app, "p", true, None);
        push_clone(&app, "c", true, Some("p"));
        let header = |key: &str| {
            let mut h = HeaderMap::new();
            h.insert(HeaderName::from_static("x-rmng-proxy-key"), key.parse().unwrap());
            h
        };

        // A top-level caller's own router key nests the new clone under it.
        let key_p = app.cliproxy.mint_router_key("p");
        assert_eq!(
            resolve_parent(&app, &json!({}), &header(&key_p)).unwrap(),
            Some("p".into())
        );
        // A sub-clone caller can't nest deeper (one level) → top-level.
        let key_c = app.cliproxy.mint_router_key("c");
        assert_eq!(resolve_parent(&app, &json!({}), &header(&key_c)).unwrap(), None);
        // An unrecognized key → top-level.
        assert_eq!(resolve_parent(&app, &json!({}), &header("bogus")).unwrap(), None);
        // An explicit `topLevel` overrides the caller key.
        assert_eq!(
            resolve_parent(&app, &json!({ "topLevel": true }), &header(&key_p)).unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn delete_cascades_to_sub_clones() {
        let app = test_app();
        // Unmanaged rows so teardown needs no Docker; the cascade wiring is what we assert.
        push_clone(&app, "p", false, None);
        push_clone(&app, "c1", false, Some("p"));
        push_clone(&app, "c2", false, Some("p"));
        push_clone(&app, "other", false, None);

        delete(State(app.clone()), Json(DeleteReq { id: "p".into() }))
            .await
            .unwrap();

        // A delete op was enqueued for the parent and each of its sub clones, but not for the
        // unrelated top-level clone.
        let ops = app.store.get().operations;
        let deleting = |id: &str| {
            ops.iter()
                .any(|o| o.target == id && o.kind == wire::OperationKind::Delete)
        };
        assert!(deleting("p") && deleting("c1") && deleting("c2"));
        assert!(!deleting("other"));
    }

    // --- POST /api/hosts/:id/mcp + /exec (the rmng desktop / exec backends) ---

    #[tokio::test]
    async fn clone_mcp_unknown_clone_is_404() {
        let app = test_app(); // no clones registered
        let err = clone_mcp(
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
    async fn clone_exec_unknown_clone_is_404() {
        let app = test_app();
        let err = clone_exec(
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
    async fn clone_exec_empty_cmd_is_400() {
        let app = test_app();
        let err = clone_exec(
            State(app.clone()),
            AxPath("anything".into()),
            Json(wire::ExecRequest::default()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("cmd"), "msg: {}", err.1);
    }

    #[tokio::test]
    async fn headless_clone_mcp_returns_conflict_with_reason() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(wire::RmngClone {
                id: "term-only".into(),
                host: "term-only".into(),
                managed: true,
                headless: true,
                ..Default::default()
            });
        });

        let err = clone_mcp(
            State(app.clone()),
            AxPath("term-only".into()),
            Json(wire::McpCallRequest {
                tool: "screenshot".into(),
                args: json!({}),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("headless"), "msg: {}", err.1);
    }

    #[tokio::test]
    async fn archived_clone_runtime_calls_return_conflict() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(wire::RmngClone {
                id: "stored".into(),
                host: "stored".into(),
                managed: true,
                archived: true,
                ..Default::default()
            });
        });

        let mcp = clone_mcp(
            State(app.clone()),
            AxPath("stored".into()),
            Json(wire::McpCallRequest {
                tool: "screenshot".into(),
                args: json!({}),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(mcp.0, StatusCode::CONFLICT);

        let chat = chat_send(
            State(app.clone()),
            AxPath("stored".into()),
            Json(ChatSendReq {
                text: "hello".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(chat.0, StatusCode::CONFLICT);

        let abort = chat_abort(State(app), AxPath("stored".into()))
            .await
            .unwrap_err();
        assert_eq!(abort.0, StatusCode::CONFLICT);
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
            detach: false,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["cmd"][0], "cat");
        assert_eq!(v["stdinB64"], "aGk=");
        assert!(v.get("stdin_b64").is_none(), "must use camelCase key");
        // `detach` is omitted when false (skip_serializing_if) and present when set.
        assert!(v.get("detach").is_none(), "detach:false must be omitted from the wire");
        let detached = serde_json::to_value(wire::ExecRequest {
            cmd: vec!["x".into()],
            detach: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(detached["detach"], true);
        // Result: exitCode maps back onto the i64 exit_code field.
        let res: wire::ExecResult =
            serde_json::from_str(r#"{ "exitCode": 3, "stdout": "out", "stderr": "err" }"#).unwrap();
        assert_eq!(res.exit_code, 3);
        assert_eq!(res.stdout, "out");
        assert_eq!(res.stderr, "err");
    }

    #[test]
    fn parse_env_lines_keeps_assignments_and_verbatim_values() {
        // Real `systemctl --user show-environment` shape: one KEY=VALUE per line, values may
        // themselves contain `=` (DBUS address) and are passed through untouched. Blank lines and
        // any non-assignment noise are dropped.
        let out = "\
WAYLAND_DISPLAY=wayland-0
DISPLAY=:0
XDG_RUNTIME_DIR=/run/user/1000
DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
PATH=/home/rmng/.local/bin:/usr/bin

not a var line
";
        let got = parse_env_lines(out);
        assert_eq!(
            got,
            vec![
                "WAYLAND_DISPLAY=wayland-0".to_string(),
                "DISPLAY=:0".to_string(),
                "XDG_RUNTIME_DIR=/run/user/1000".to_string(),
                "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus".to_string(),
                "PATH=/home/rmng/.local/bin:/usr/bin".to_string(),
            ]
        );
    }

    #[test]
    fn merge_env_lets_caller_override_and_appends_new() {
        let mut base = vec![
            "WAYLAND_DISPLAY=wayland-0".to_string(),
            "PATH=/session/bin".to_string(),
            "XDG_RUNTIME_DIR=/run/user/1000".to_string(),
        ];
        // Caller overrides PATH and adds a brand-new key; the untouched session vars remain.
        merge_env(&mut base, &["PATH=/caller/bin".to_string(), "FOO=1".to_string()]);
        assert_eq!(
            base,
            vec![
                "WAYLAND_DISPLAY=wayland-0".to_string(),
                "XDG_RUNTIME_DIR=/run/user/1000".to_string(),
                "PATH=/caller/bin".to_string(),
                "FOO=1".to_string(),
            ]
        );
    }

    #[test]
    fn desktop_user_detection() {
        assert!(is_desktop_user("1000"));
        assert!(is_desktop_user("rmng"));
        assert!(!is_desktop_user("root"));
        assert!(!is_desktop_user("0"));
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

        // A clone with no notes yet reads back an empty `blocks` array (not a bare `[]`).
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

    // The `/cc` router's own tests moved to `groupproxy.rs` along with the router itself.
    // What stays here is the control-server half of the boundary: the internal token-delta
    // intake's auth.

    /// The token-delta intake must reject anything that isn't the shared admin secret — it
    /// mutates durable per-clone accounting, and the group-proxy is the only legitimate caller.
    #[tokio::test]
    async fn internal_tokens_rejects_a_wrong_or_missing_admin_key() {
        let app = test_app();
        let secret = app.cliproxy.ensure_admin_secret();
        let delta = crate::tokens::TokenDelta {
            host_id: "h1".into(),
            epoch: 1,
            input: 5,
            ..Default::default()
        };

        let err = internal_tokens(State(app.clone()), HeaderMap::new(), Json(vec![delta.clone()]))
            .await
            .expect_err("no admin key must be rejected");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);

        let mut wrong = HeaderMap::new();
        wrong.insert(
            HeaderName::from_static(crate::groupproxy::ADMIN_HEADER),
            "nope".parse().unwrap(),
        );
        let err = internal_tokens(State(app.clone()), wrong, Json(vec![delta.clone()]))
            .await
            .expect_err("a wrong admin key must be rejected");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);

        // The real secret is accepted. (The delta names a clone with no token record, so it is
        // dropped by the epoch guard — this asserts the auth gate, not the accounting.)
        let mut ok = HeaderMap::new();
        ok.insert(
            HeaderName::from_static(crate::groupproxy::ADMIN_HEADER),
            secret.parse().unwrap(),
        );
        assert_eq!(
            internal_tokens(State(app), ok, Json(vec![delta])).await.unwrap(),
            StatusCode::NO_CONTENT
        );
    }

    /// A delta that arrives over the boundary must land on the same durable totals the
    /// in-process observer used to write — and a delta stamped with a stale lifecycle epoch
    /// must be dropped, exactly as `TokenBus::record` dropped a stale in-process observer.
    #[tokio::test]
    async fn internal_tokens_applies_current_epoch_and_drops_stale() {
        let app = test_app();
        let secret = app.cliproxy.ensure_admin_secret();
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(crate::groupproxy::ADMIN_HEADER),
            secret.parse().unwrap(),
        );
        app.store.mutate(|s| {
            s.hosts.push(wire::RmngClone {
                id: "h1".into(),
                host: "h1".into(),
                managed: true,
                ..Default::default()
            })
        });
        app.tokens.register_clone("h1");
        let epoch = app.tokens.capture_epoch("h1").expect("an active clone has an epoch");

        internal_tokens(
            State(app.clone()),
            headers.clone(),
            Json(vec![crate::tokens::TokenDelta {
                host_id: "h1".into(),
                epoch,
                input: 7,
                output: 3,
                count_request: true,
                ..Default::default()
            }]),
        )
        .await
        .unwrap();
        assert!(app.tokens.last_token_at("h1").is_some(), "the delta was applied");

        // Archive/unarchive advances the epoch; the pre-transition epoch must no longer apply.
        app.tokens.set_archived("h1", true);
        app.tokens.set_archived("h1", false);
        let before = app.tokens.last_token_at("h1");
        internal_tokens(
            State(app.clone()),
            headers,
            Json(vec![crate::tokens::TokenDelta {
                host_id: "h1".into(),
                epoch,
                input: 1_000_000,
                ..Default::default()
            }]),
        )
        .await
        .unwrap();
        assert_eq!(app.tokens.last_token_at("h1"), before, "a stale epoch changes nothing");
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
        assert!(
            seen.contains("event:stats"),
            "no stats snapshot in: {buf:?}"
        );
        assert!(
            seen.contains("event:forwards"),
            "no forwards snapshot in: {buf:?}"
        );
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
    use wire::{ControlState, RmngClone};

    fn state_with(hosts: Vec<RmngClone>) -> ControlState {
        ControlState {
            hosts,
            ..Default::default()
        }
    }

    fn host(id: &str) -> RmngClone {
        RmngClone {
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
    fn rejects_local_port_used_by_another_clone() {
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

    // ---- compose_global_prompt (layers a + c) ----

    fn cfg_global(a: &str) -> wire::AppConfig {
        wire::AppConfig {
            global_prompt: a.into(),
            ..Default::default()
        }
    }
    fn preset_global(c: &str) -> wire::Preset {
        wire::Preset {
            name: "p".into(),
            global_prompt: c.into(),
            ..Default::default()
        }
    }

    #[test]
    fn global_prompt_is_a_then_c() {
        // a only (no preset / empty c) → just a; a+c → joined with a blank line.
        assert_eq!(compose_global_prompt(&cfg_global("A"), None), "A");
        assert_eq!(
            compose_global_prompt(&cfg_global("A"), Some(&preset_global("   "))),
            "A"
        );
        assert_eq!(
            compose_global_prompt(&cfg_global("A"), Some(&preset_global("C"))),
            "A\n\nC"
        );
    }

    // --- the /cc tombstone ---------------------------------------------------------------

    /// A pre-split clone's still-running agent POSTs to the old `/cc` path. The whole point of
    /// the tombstone is that it must NOT look like a page: an agent SDK parsing `200 text/html`
    /// (the SPA fallback) reports an unintelligible error instead of a retryable one.
    #[tokio::test]
    async fn cc_tombstone_answers_json_not_the_spa_fallback() {
        let resp = cc_moved().await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let ctype = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(ctype.contains("application/json"), "content-type was {ctype:?}");
        // Retryable + points at where it moved, so a manual follower can get there.
        assert_eq!(
            resp.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()),
            Some(crate::groupproxy::cc_base_url().as_str())
        );
        assert!(resp.headers().contains_key(header::RETRY_AFTER));

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).expect("a JSON error body");
        assert_eq!(v["type"], "error");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        // Names the new host so the message alone is enough to diagnose it.
        assert!(msg.contains(crate::groupproxy::CONTAINER), "message was {msg:?}");
    }
}
