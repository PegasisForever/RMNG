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
        .route("/api/activate", post(activate))
        .route("/api/board", put(board_put))
        .route("/api/tickets", post(ticket_post))
        // Ahead of `/api/tickets/:id` for the reader's sake, not the router's: axum matches
        // a literal segment before a parameter whatever order they were added in, and
        // `ticket_order_put_round_trips_over_http` is what proves it.
        .route("/api/tickets/order", put(ticket_order_put))
        .route("/api/tickets/:id", put(ticket_put))
        .route("/api/clone", post(clone))
        .route("/api/layout/activate", post(layout_activate))
        .route("/api/delete", post(delete))
        .route("/api/notes/:id", get(notes_get).put(notes_save))
        .route("/api/upload", post(upload))
        .route("/api/linear/upload-relay", post(linear_upload_relay))
        .route("/api/linear/asset", get(linear_asset))
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
        .route("/api/hosts/:id/archive", post(archive))
        .route("/api/hosts/:id/unarchive", post(unarchive))
        .route("/api/hosts/:id/mcp", post(clone_mcp))
        .route("/api/hosts/:id/exec", post(clone_exec))
        // Claude + Codex accounts. The server owns each account's OAuth refresh lifecycle and
        // pushes only short-lived access tokens into clones; these twelve are symmetric across
        // the two providers. Account POOLS are not edited here — they live in `config.json`
        // (`cloneGroups`/`codexGroups`) and are saved wholesale through `PUT /api/config`.
        .route("/api/claude/import/check", post(claude_import_check))
        .route("/api/claude/import", post(claude_import))
        .route("/api/claude/refresh", post(claude_refresh))
        .route("/api/claude/swap", post(claude_swap))
        .route("/api/claude/delete", post(claude_delete))
        .route("/api/claude/rotate", post(claude_rotate))
        .route("/api/codex/import/check", post(codex_import_check))
        .route("/api/codex/import", post(codex_import))
        .route("/api/codex/refresh", post(codex_refresh))
        .route("/api/codex/swap", post(codex_swap))
        .route("/api/codex/delete", post(codex_delete))
        .route("/api/codex/rotate", post(codex_rotate));

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

    // Who the browser is talking to, sent once per connection. A page that sees this change
    // between connections is running a bundle from a server that no longer exists, so it
    // reloads itself. An upgrade drops every SSE connection, which is what makes one frame on
    // connect enough — there is no need to repeat it.
    let build_id = json!({ "buildId": app.build_id() }).to_string();
    let version_stream =
        futures::stream::once(
            async move { Ok(Event::default().event("version").data(build_id)) },
        );

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
                futures::stream::select(stats_stream, lxc_stream),
                futures::stream::select(fwd_stream, version_stream),
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

#[derive(Deserialize)]
struct ActivateReq {
    #[serde(default)]
    id: Option<String>,
}

async fn activate(State(app): State<App>, Json(req): Json<ActivateReq>) -> Json<ControlState> {
    // An archived clone cannot be selected. Selecting one points the viewer's input at a
    // frozen daemon that will never read its socket, and every pointer move then fills a
    // queue nobody drains. Clicking an archived card leaves the selection where it was
    // rather than failing, since the click is a navigation, not a command.
    let req = match req.id.as_deref() {
        Some(id)
            if app.store.get().hosts.iter().any(|h| h.id == id && h.archived) =>
        {
            ActivateReq { id: app.store.get().selected }
        }
        _ => req,
    };
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
#[serde(rename_all = "camelCase")]
struct BoardPutReq {
    columns: Vec<wire::BoardColumn>,
}

/// Replace the board's columns wholesale. The frontend owns the layout rules (which clone
/// sits where, what a delete does with the leftovers) and sends the settled list, so there
/// is nothing to merge here.
///
/// A clone id may appear in more than one column only by a client bug; the board draws the
/// first occurrence and drops the rest, so this stores what it is given rather than
/// second-guessing it.
async fn board_put(State(app): State<App>, Json(req): Json<BoardPutReq>) -> Json<ControlState> {
    Json(app.store.mutate(|s| s.board_columns = req.columns))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TicketOrderPutReq {
    ticket_ids: Vec<String>,
}

/// Replace the operator's ticket order wholesale. Same bargain as [`board_put`]: the client
/// owns the arrangement and sends the settled list, so there is nothing to merge here.
///
/// Ids are lowercased on the way in. The browser already compares them case-insensitively,
/// so mixed case would still draw correctly, but it would leave `state.json` holding a case
/// its only reader never looks at.
///
/// Ids for tickets that no longer exist are kept, not filtered. This server no longer knows
/// which tickets exist, and an entry that matches nothing costs one string.
async fn ticket_order_put(
    State(app): State<App>,
    Json(req): Json<TicketOrderPutReq>,
) -> Json<ControlState> {
    let order: Vec<String> = req.ticket_ids.iter().map(|id| id.to_lowercase()).collect();
    Json(app.store.mutate(|s| s.ticket_order = order))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TicketPutReq {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// `PUT /api/tickets/:id` — write a title and/or description back to Linear.
///
/// `:id` is the human identifier (`WE-142`); [`crate::linear::update_issue`] resolves it to
/// Linear's own id and picks the key that can see it. The browser holds a Linear key of its
/// own now and will make this call directly, so this route is on its way out.
///
/// On success the ticket in `ControlState` is patched immediately rather than waiting for
/// the next poll. The poll is a minute away, and a description that snaps back to its old
/// text for that minute reads as the save having failed.
async fn ticket_put(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<TicketPutReq>,
) -> Result<Json<ControlState>, (StatusCode, String)> {
    let cfg = app.config();
    let keys: Vec<&str> = cfg.presets.iter().map(|p| p.linear_key.as_str()).collect();
    crate::linear::update_issue(
        &app.http,
        &keys,
        &id,
        req.title.as_deref(),
        req.description.as_deref(),
    )
    .await
    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    Ok(Json(app.store.mutate(|s| {
        if let Some(t) = s.tickets.iter_mut().find(|t| t.id.eq_ignore_ascii_case(&id)) {
            if let Some(title) = req.title {
                t.title = title;
            }
            if let Some(description) = req.description {
                t.description = (!description.is_empty()).then_some(description);
            }
        }
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TicketPostReq {
    /// Team key, e.g. `WE`. Case-insensitive: it also names the preset whose key opens it.
    team: String,
    title: String,
    #[serde(default)]
    description: String,
    /// Linear's scale: 1 urgent, 2 high, 3 medium, 4 low. Absent or 0 means none.
    #[serde(default)]
    priority: Option<u8>,
}

/// `POST /api/tickets` — open a new Linear issue from the ticket column.
///
/// The team key picks the preset, exactly as the clone dialog's "new ticket" tab does: a
/// preset labelled `WE` owns team WE, and its Linear key is the one that opens the issue.
/// The browser names the team and the server does the rest. It holds a key of its own now
/// and will make this call directly, so this route is on its way out.
///
/// The new ticket is prepended to `ControlState` rather than waited for. The poll is a
/// minute away, and a column that stays empty for that minute reads as the create having
/// failed.
async fn ticket_post(
    State(app): State<App>,
    Json(req): Json<TicketPostReq>,
) -> Result<Json<wire::LinearTicket>, (StatusCode, String)> {
    let team = req.team.trim().to_string();
    let title = req.title.trim().to_string();
    if team.is_empty() || title.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "a team key and a title are required".into()));
    }
    let cfg = app.config();
    let preset = linear::pick_preset_by_prefix(&cfg.presets, &team).ok_or((
        StatusCode::BAD_REQUEST,
        format!("no preset is labelled {team}, so no Linear key can open a ticket there"),
    ))?;
    let ticket = linear::create_ticket(
        &app.http,
        &preset.linear_key,
        &team,
        &title,
        &req.description,
        req.priority,
    )
    .await
    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    let published = ticket.clone();
    app.store.mutate(move |s| {
        s.tickets.retain(|t| !t.id.eq_ignore_ascii_case(&published.id));
        s.tickets.insert(0, published.clone());
    });
    Ok(Json(ticket))
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
/// [`crate::clonekey::CloneKeys::clone_for_token`]) and nest under it only when the caller
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
        .and_then(|key| app.clone_keys.clone_for_token(key));
    Ok(caller.filter(|id| top_level_managed(id)))
}

/// The effective account selections + preset for a fleet-CLI clone, applying sub-clone
/// inheritance: a sub clone inherits its `parent`'s accounts / preset unless the request
/// specified them (an explicit `--claude-account`/`--codex-account`/`--preset`, including
/// `none`, counts as specified and overrides). No parent, or a parent with nothing to inherit,
/// yields `None` — which the account layer reads as "auto". Pure — unit-tested. The returned
/// preset borrows `presets` (the live config preset list).
fn effective_accounts_preset<'a>(
    parent: Option<&wire::RmngClone>,
    claude_specified: bool,
    claude_account: Option<String>,
    codex_specified: bool,
    codex_account: Option<String>,
    preset_specified: bool,
    explicit: Option<&'a wire::Preset>,
    presets: &'a [wire::Preset],
) -> (Option<String>, Option<String>, Option<&'a wire::Preset>) {
    let preset = if preset_specified {
        explicit
    } else {
        parent
            .and_then(|h| h.preset_name.as_deref())
            .and_then(|name| presets.iter().find(|p| p.name == name))
    };
    // Per provider, strongest first:
    //   1. an explicit selection on the request,
    //   2. the parent clone's selection (sub clone),
    //   3. the effective preset's default,
    //   4. nothing — which the account layer reads as `auto`.
    //
    // What is inherited at step 2 is the *selection*, not the resolved account: the parent may be
    // on `auto` and have landed on a specific email, and a sub clone asking for `auto` should get
    // its own pick rather than being pinned to whatever its parent happens to be running.
    //
    // A BLANK preset default is skipped rather than treated as a choice, which is what lets the
    // chain reach step 4 — an explicit `none` on a preset is a real decision (boot tokenless) and
    // must not be confused with having no opinion.
    let claude = if claude_specified {
        claude_account
    } else {
        parent
            .and_then(|h| h.claude_selection.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                preset
                    .map(|p| p.claude_account.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
    };
    let codex = if codex_specified {
        codex_account
    } else {
        parent
            .and_then(|h| h.codex_selection.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                preset
                    .map(|p| p.codex_account.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
    };
    (claude, codex, preset)
}

/// Repoint any clone bound to a pool that this config save deleted, per provider.
///
/// A clone's pool binding is `claude_group` + a `claude_selection` of `group:<name>`. When the
/// pool is gone, both are meaningless: the rotator skips unknown groups, so the clone is frozen
/// on its last account forever. `auto` is the honest replacement — the clone keeps working and
/// rejoins normal rotation across every imported account — and it is what the operator would
/// get had they never named a pool.
///
/// Deliberately NOT a reconciler pass: doing it here means it happens the instant the pool is
/// deleted, and only for pools that actually disappeared in THIS save. A periodic sweep would
/// also "heal" a clone whose pool is merely absent because config failed to load.
fn heal_dangling_pool_bindings(app: &App, old: &wire::AppConfig, merged: &wire::AppConfig) {
    let gone = |before: &[wire::CloneGroup], after: &[wire::CloneGroup]| -> Vec<String> {
        before
            .iter()
            .filter(|b| !after.iter().any(|a| a.name == b.name))
            .map(|b| b.name.clone())
            .collect()
    };
    let claude_gone = gone(&old.clone_groups, &merged.clone_groups);
    let codex_gone = gone(&old.codex_groups, &merged.codex_groups);
    if claude_gone.is_empty() && codex_gone.is_empty() {
        return;
    }
    let mut healed: Vec<(String, &'static str, String)> = Vec::new();
    app.store.mutate(|s| {
        for h in s.hosts.iter_mut() {
            if let Some(g) = h.claude_group.clone().filter(|g| claude_gone.contains(g)) {
                h.claude_group = None;
                h.claude_selection = Some("auto".to_string());
                healed.push((h.id.clone(), "claude", g));
            }
            if let Some(g) = h.codex_group.clone().filter(|g| codex_gone.contains(g)) {
                h.codex_group = None;
                h.codex_selection = Some("auto".to_string());
                healed.push((h.id.clone(), "codex", g));
            }
        }
    });
    for (clone, provider, pool) in healed {
        tracing::info!(
            "clone {clone}: {provider} pool {pool:?} was deleted — repointed at `auto` so it \
             keeps rotating instead of freezing on its current account"
        );
    }
}

/// One provider's account selection for the plain / ticket create modes, which have no parent to
/// inherit from: an explicit request value, else the resolved preset's default, else `None`
/// (read as `auto` downstream).
///
/// Split out so all three create modes agree on the preset step — hostname mode reaches it
/// through [`effective_accounts_preset`]'s longer chain, and these two would otherwise silently
/// ignore a preset's default.
fn account_or_preset_default(
    requested: Option<&String>,
    preset: Option<&wire::Preset>,
    pick: impl Fn(&wire::Preset) -> &str,
) -> Option<String> {
    if let Some(a) = requested {
        return Some(a.clone());
    }
    preset
        .map(|p| pick(p).trim().to_string())
        .filter(|s| !s.is_empty())
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
    // Account selections, verbatim as the operator wrote them: an email, `auto`, `none`, or
    // `group:<pool>`.
    //
    // A named POOL is validated here, at the boundary, because the assign-time path cannot fail
    // usefully: `claude::pick_group_account` returns `None` for an unknown pool, which makes
    // `resolve_assignment` return `None`, which makes `jobs::run_clone` skip the whole account
    // step — producing a running, tokenless clone and not one line of explanation. A typo'd
    // `--claude-account group:pooed` should not cost you a silent clone.
    //
    // An unknown *email* is deliberately NOT rejected: that path warns and falls back to the
    // best-scored account, which is a recoverable outcome the operator can see in the account
    // column, and rejecting it would break `auto`-style workflows against a not-yet-imported
    // address.
    let claude_account = str_field("claudeAccount");
    let codex_account = str_field("codexAccount");
    let cfg_pools = app.config();
    let check_pool = |sel: Option<&String>, pools: &[wire::CloneGroup], flag: &str| {
        let Some(name) = sel.and_then(|s| s.trim().strip_prefix("group:")) else {
            return Ok(());
        };
        let name = name.trim();
        if pools.iter().any(|p| p.name == name) {
            return Ok(());
        }
        let known: Vec<&str> = pools.iter().map(|p| p.name.as_str()).collect();
        Err(bad(format!(
            "unknown {flag} pool '{name}' (configured: {})",
            if known.is_empty() { "none".to_string() } else { known.join(", ") }
        )))
    };
    check_pool(claude_account.as_ref(), &cfg_pools.clone_groups, "claudeAccount")?;
    check_pool(codex_account.as_ref(), &cfg_pools.codex_groups, "codexAccount")?;
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
        let (eff_claude, eff_codex, eff_preset) = effective_accounts_preset(
            parent_clone.as_ref(),
            claude_account.is_some(),
            claude_account.clone(),
            codex_account.is_some(),
            codex_account.clone(),
            preset_specified,
            explicit,
            &cfg.presets,
        );
        let spec = CloneSpec {
            source_image: image,
            new_hostname: hostname,
            linear: None,
            claude_account: eff_claude,
            codex_account: eff_codex,
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
            claude_account: account_or_preset_default(
                claude_account.as_ref(),
                explicit,
                |p| &p.claude_account,
            ),
            codex_account: account_or_preset_default(
                codex_account.as_ref(),
                explicit,
                |p| &p.codex_account,
            ),
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
        // Ticket mode's preset may have been label-auto-selected, so its account defaults are
        // only knowable here, after `resolve_issue`.
        claude_account: account_or_preset_default(claude_account.as_ref(), Some(&preset), |p| {
            &p.claude_account
        }),
        codex_account: account_or_preset_default(codex_account.as_ref(), Some(&preset), |p| {
            &p.codex_account
        }),
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
        app.clone_keys.forget(child);
        app.claude.forget_pushed(child);
        app.codex.forget_pushed(child);
        if let Err(e) = jobs::start_delete(&app, child) {
            tracing::warn!(target: "clone", "cascade delete of sub clone '{child}' skipped: {e}");
        }
    }
    // Drop the clone's group-proxy router key so a stale bearer can never route again.
    app.clone_keys.forget(&req.id);
    app.claude.forget_pushed(&req.id);
    app.codex.forget_pushed(&req.id);
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

// --- the Linear upload relay -----------------------------------------------
//
// One hop, for one reason: a browser can run Linear's `fileUpload` mutation but cannot PUT to
// the Google-signed URL it answers with. Measured twice. The bucket answers the preflight
// with HTTP 200 and `vary: Origin` alone, no `access-control-allow-origin`, so the request
// never leaves the page.
//
// So the browser posts the bytes here with the URL and the headers Linear handed it, and this
// replays the PUT. It never calls Linear's GraphQL and holds no key: the mutation, the signed
// URL, and the `assetUrl` that ends up in the markdown all stay in the browser. That is what
// keeps the control-server out of the Linear business while the bytes still get through.

/// The only host this route will PUT to.
///
/// Not defence in depth, the whole defence. The bucket returns 200 whatever `Origin` and
/// `Referer` say, this port carries no authentication, and the URL comes from the request. An
/// allowlist is the one thing between that and a general outbound proxy.
const UPLOAD_RELAY_HOST: &str = "storage.googleapis.com";

/// How long the PUT may take before this gives up on it.
///
/// Linear signs the URL with `X-Goog-Expires=60`, and a PUT 68 seconds after issue comes back
/// 400 `ExpiredToken`. The browser has already spent part of that minute on the mutation and
/// on posting the bytes here, so a hop still running at 45 seconds has missed the window. The
/// caller is better served by this sentence than by waiting for the bucket to say so.
const UPLOAD_RELAY_TIMEOUT_SECS: u64 = 45;

/// Headers that describe this hop rather than the upload, and would break the replayed PUT.
///
/// `content-length` is reqwest's to set from the body it is given. `host` decides which
/// virtual host the request lands on, which would undo the allowlist above. The other two are
/// hop-by-hop by definition.
const UPLOAD_RELAY_DROPPED: [&str; 4] = ["host", "content-length", "transfer-encoding", "connection"];

/// How much of the `file` field to hold in memory while the `url` field has not arrived yet.
///
/// Field order is the client's to choose, and this route has no authentication in front of
/// it, so a body aimed at a host that will be refused must not be able to cost what a body
/// aimed at an accepted one costs. Four concurrent 60MB posts at a rejected host used to take
/// the process from 35MB to 212MB resident.
///
/// Reading the target first makes that refusal free in the order our own `upload.ts` sends
/// (`url`, `headers`, `file`), measured at 12KB of growth for the same four. This bounds the
/// orders it does not send, measured at 4MB: one cap per request in flight.
const UPLOAD_RELAY_PREBUFFER: usize = 1024 * 1024;

/// The most one `url` field may be. A signed Google upload URL runs to about 700 bytes, so
/// this leaves room for a much longer one and none for a payload wearing a target's name.
const UPLOAD_RELAY_URL_MAX: usize = 8 * 1024;

/// The most one `headers` field may be. Linear signs five short pairs into it.
const UPLOAD_RELAY_HEADERS_MAX: usize = 64 * 1024;

/// One text field, read a chunk at a time and refused past `cap`.
///
/// [`Field::text`] has no bound short of the 64MB `DefaultBodyLimit`, so a caller could spend
/// 60MB in `url` or in `headers` and never send a `file` at all. Measured: 60MB in `url` took
/// the process from 19064kB to 146280kB of VmHWM, and 60MB in `headers` with no `url` field at
/// all took it from 19244kB to 145720kB. Every field this route reads is bounded, here or by
/// the `file` arm's own cap, and the fields it does not read are drained rather than held.
async fn field_text(
    field: &mut axum::extract::multipart::Field<'_>,
    name: &str,
    cap: usize,
) -> Result<String, String> {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|e| e.to_string())? {
        if buf.len() + chunk.len() > cap {
            return Err(format!(
                "the '{name}' field is larger than the {cap} bytes this route reads"
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8(buf).map_err(|_| format!("the '{name}' field is not text"))
}

/// Refuse any URL that is not plain https at exactly `allowed`.
///
/// Host, scheme, and port all have to match: `storage.googleapis.com.example.net` is a
/// different host, `http` is a downgrade, and a port is a different listener on the same name.
///
/// `reqwest::Url::parse` is what decides the host, which is the same parser reqwest itself
/// will use to pick the connection. That is deliberate and it is what removes the whole class
/// of parser-differential bypass: `evil.tld@storage.googleapis.com` has host
/// `storage.googleapis.com` here AND connects there, `STORAGE.GOOGLEAPIS.COM` is lowercased
/// by the same IDNA pass both sides run, and a trailing dot stays in the string both sides
/// compare. A second parser here, however careful, could disagree with the one that dials.
///
/// Shared by the two routes that fetch a URL the caller chose. `allowed` is passed in rather
/// than read from a list, so each route names its one host at its own call site and widening
/// one can never widen the other.
fn check_outbound_host(url: &str, allowed: &str, label: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|e| format!("{label} is not a URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err(format!("{label} must be https, got {}", parsed.scheme()));
    }
    if let Some(port) = parsed.port() {
        return Err(format!("{label} must use the default https port, got {port}"));
    }
    match parsed.host_str() {
        Some(host) if host == allowed => Ok(()),
        Some(host) => Err(format!("{label} host must be {allowed}, got {host}")),
        None => Err(format!("{label} has no host")),
    }
}

/// Refuse any target but the signed-upload bucket.
fn check_relay_target(url: &str) -> Result<(), String> {
    check_outbound_host(url, UPLOAD_RELAY_HOST, "upload url")
}

/// The headers to replay, out of the `[{ "key": ..., "value": ... }]` the browser sent.
///
/// Forwarded verbatim, because the signature covers them. Substituting
/// `application/octet-stream` for the declared `content-type` returns 403
/// `SignatureDoesNotMatch`, and one byte outside `x-goog-content-length-range` returns 400
/// `EntityTooLarge`. Both were observed, not read.
///
/// A malformed entry is an error rather than a skip, for the same reason: a signed header
/// dropped quietly comes back as a 403 with nothing pointing at the cause.
fn relay_headers(raw: &str) -> Result<Vec<(String, String)>, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("headers is not JSON: {e}"))?;
    let entries = parsed
        .as_array()
        .ok_or_else(|| "headers must be a JSON array".to_string())?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let key = entry.get("key").and_then(|v| v.as_str());
        let value = entry.get("value").and_then(|v| v.as_str());
        let (key, value) = match (key, value) {
            (Some(k), Some(v)) if !k.trim().is_empty() => (k.trim(), v),
            _ => return Err(format!("headers entry is not {{key, value}}: {entry}")),
        };
        if UPLOAD_RELAY_DROPPED.contains(&key.to_ascii_lowercase().as_str()) {
            continue;
        }
        out.push((key.to_string(), value.to_string()));
    }
    Ok(out)
}

/// The PUT, built but not sent: the target, every header to replay, and this hop's deadline.
///
/// Separate from the handler so a test can point it at a stub and read back what arrived. The
/// forwarding is the part with no margin in it. A header changed on the way through is a 403
/// the caller cannot see the cause of.
fn relay_request(
    http: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    let mut req = http
        .put(url)
        .timeout(Duration::from_secs(UPLOAD_RELAY_TIMEOUT_SECS));
    for (key, value) in headers {
        req = req.header(key, value);
    }
    req
}

/// What to tell the operator when the bucket refuses.
///
/// Google answers with an XML body whose `<Code>` names the reason, and the three that this
/// route can actually produce are worth saying plainly: an expired URL is a retry, a bad
/// signature or a size mismatch is a bug here.
fn relay_failure(status: u16, body: &str) -> String {
    match xml_tag(body, "Code").as_deref() {
        Some("ExpiredToken") => format!(
            "the signed upload URL expired before the bytes reached the bucket \
             (HTTP {status} ExpiredToken). Linear signs it for 60 seconds. Paste it again."
        ),
        Some(code) => format!("the storage bucket refused the upload (HTTP {status} {code})"),
        None => format!("the storage bucket refused the upload (HTTP {status})"),
    }
}

/// The text inside the first `<tag>…</tag>` of an XML body, when it holds one.
fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_string())
}

/// `POST /api/linear/upload-relay`: replay one PUT to a Google-signed upload URL.
///
/// Multipart, three fields: `url` is the `uploadUrl` Linear signed, `headers` is its
/// `headers[]` as JSON with the declared `content-type` in front, and `file` is the bytes.
///
/// The target is checked the moment it is read, before the bytes are buffered, so a request
/// aimed at a host this will refuse is refused for the price of one string. See
/// [`UPLOAD_RELAY_PREBUFFER`] for the order the client does not send.
///
/// Every field is bounded, and each of the three is read at most once. A second `url` is what
/// lets a caller spend a whole `file` under a target that was allowed and then name a
/// different one, so it is refused rather than overwritten.
async fn linear_upload_relay(
    State(app): State<App>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let dup = |name: &str| (StatusCode::BAD_REQUEST, format!("the '{name}' field was sent twice"));
    let mut url = String::new();
    // Set once `url` has arrived AND passed [`check_relay_target`]: a failed check returns, so
    // the two are the same fact. It gates the `file` cap below and refuses a second `url`.
    let mut allowed = false;
    let mut headers_raw: Option<String> = None;
    let mut body: Option<Vec<u8>> = None;

    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| bad(e.to_string()))?
    {
        match field.name().unwrap_or("") {
            "url" => {
                if allowed {
                    return Err(dup("url"));
                }
                url = field_text(&mut field, "url", UPLOAD_RELAY_URL_MAX).await.map_err(bad)?;
                check_relay_target(&url).map_err(bad)?;
                allowed = true;
            }
            "headers" => {
                if headers_raw.is_some() {
                    return Err(dup("headers"));
                }
                headers_raw = Some(
                    field_text(&mut field, "headers", UPLOAD_RELAY_HEADERS_MAX)
                        .await
                        .map_err(bad)?,
                );
            }
            "file" => {
                if body.is_some() {
                    return Err(dup("file"));
                }
                // Streamed rather than `field.bytes()`, which has no bound short of the 64MB
                // body limit. A file that arrives before its target is capped: nothing has
                // agreed to carry it anywhere yet.
                let mut buf: Vec<u8> = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(|e| bad(e.to_string()))? {
                    if !allowed && buf.len() + chunk.len() > UPLOAD_RELAY_PREBUFFER {
                        return Err(bad(format!(
                            "send the 'url' field before the 'file' field: over \
                             {}MB arrived with no upload target to check",
                            UPLOAD_RELAY_PREBUFFER / (1024 * 1024)
                        )));
                    }
                    buf.extend_from_slice(&chunk);
                }
                body = Some(buf);
            }
            // A field this route does not read still arrives. Drained a chunk at a time so
            // that naming one `payload` costs the same as naming it nothing.
            _ => while field.chunk().await.map_err(|e| bad(e.to_string()))?.is_some() {},
        }
    }

    // Reached only when `url` came last or never came at all. A missing field lands here as
    // an empty string, which is not a URL, so the message still names the field.
    if !allowed {
        check_relay_target(&url).map_err(bad)?;
    }
    let body = body.ok_or_else(|| bad("no 'file' field".into()))?;
    // Absent means no headers at all, which the bucket answers with a 403. That is the caller's
    // mistake to make, and reading it as "send nothing" beats guessing a content type.
    let headers_raw = headers_raw.unwrap_or_default();
    let headers = if headers_raw.trim().is_empty() {
        Vec::new()
    } else {
        relay_headers(&headers_raw).map_err(bad)?
    };

    let resp = relay_request(&app.relay_http, url.trim(), &headers)
        .body(body)
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("the upload never reached the storage bucket: {e}"),
            )
        })?;

    let status = resp.status().as_u16();
    if resp.status().is_redirection() {
        let to = redirect_target(resp.headers());
        tracing::warn!("linear upload relay: bucket answered {status} -> {to}, refusing to follow");
        return Err((StatusCode::BAD_GATEWAY, relay_redirect_refused(status, &to)));
    }
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("linear upload relay: bucket answered {status}: {text}");
        return Err((StatusCode::BAD_GATEWAY, relay_failure(status, &text)));
    }
    Ok(Json(json!({ "ok": true, "status": status })))
}

/// Where a 30x pointed, for a log line and an error message. `?` when it carried no `Location`.
fn redirect_target(headers: &HeaderMap) -> String {
    headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("?")
        .to_string()
}

/// What the operator is told when an allowlisted host answers with a redirect.
///
/// The host check runs once, against the url the caller sent. Following a 301, 302, or 307
/// would replay the PUT, its body, and its signed headers at whatever the `Location` names,
/// on a port with no authentication on any route. A redirect is a dead end here by
/// construction ([`App::relay_http`]), and this sentence says so rather than reading as a
/// bucket error.
fn relay_redirect_refused(status: u16, location: &str) -> String {
    format!(
        "the upload target answered HTTP {status} with a redirect to {location}, and this \
         route does not follow redirects: only the first URL is checked against the \
         {UPLOAD_RELAY_HOST} allowlist, so a second one is never sent"
    )
}

// --- Linear asset proxy ----------------------------------------------------
//
// An image pasted into a ticket body now lives in Linear, because the body itself does: a
// `/uploads/<name>` URL resolves on this LAN and nowhere else, so an issue carrying one is a
// broken image for everyone but us. The cost is the mirror image of that problem. An
// unauthenticated GET on a Linear `assetUrl` answers:
//
//     HTTP/2 401
//     {"error":"unauthorized","message":"Please provide authorization header compatible
//      with Linear GraphQL API"}
//
// with no redirect. An `<img>` cannot send that header, and the browser cannot fetch the bytes
// itself either: `uploads.linear.app` answers a preflight with `access-control-allow-headers:
// range`, and `authorization` is not in it. Both measured.
//
// So this route is the one Linear call the control-server keeps, and it is a read: it fetches
// the asset with a preset's key and hands the bytes back same-origin, which is a source an
// `<img>` can use. The markdown saved to Linear still carries the original `assetUrl`. The
// rewrite to this route happens in the browser, at render time.

/// The only host this route will GET from. Separate constant from [`UPLOAD_RELAY_HOST`] on
/// purpose: two routes, two allowlists, one host each.
const LINEAR_ASSET_HOST: &str = "uploads.linear.app";

/// The most one asset may be served as. Counted as the bytes arrive, so a body that declares
/// no length or lies about one is cut off at the same place a declared one is refused.
const LINEAR_ASSET_MAX_BYTES: usize = 32 * 1024 * 1024;

/// How long the WHOLE request may take: every key attempt and the body, on one clock.
///
/// Per attempt this is a multiplier, not a bound. With two keys against a target that never
/// answers, a per-attempt 20 seconds measured 40.002s of held connection, and five presets
/// would be 100s. The route has no authentication in front of it, so the number a caller can
/// hold a socket for has to be the number written here and not a multiple of it.
const LINEAR_ASSET_TIMEOUT_SECS: u64 = 20;

/// Refuse any target but Linear's asset host.
fn check_asset_target(url: &str) -> Result<(), String> {
    check_outbound_host(url, LINEAR_ASSET_HOST, "asset url")
}

/// The content types this route serves, and the exact string each is served as.
///
/// A closed table, never a passthrough. Linear stores whatever `contentType` the uploader
/// declared and hands that back on a read, so the upstream header is chosen by whoever put the
/// file there. A real asset in a real workspace answers:
///
///     HTTP/2 200
///     content-type: text/html
///     content-disposition: attachment; filename="linear_download.html"
///     content-security-policy: sandbox
///
/// Forwarding that `content-type` put `<script>alert(document.domain)</script>` on an origin
/// that serves `/api/hosts/:id/exec`, `/api/clone`, `/api/delete`, and `PUT /api/config`, none
/// of which ask for a credential. `nosniff` is no help at all when the declared type IS
/// `text/html`: it stops a browser guessing, and nothing here was a guess.
///
/// The same shape as [`files::mime_for_ext`], which decides `/uploads/:file`, with one
/// difference: no `image/svg+xml`. An SVG is a document that draws, scripts included, and this
/// origin has no authentication on any route. A stored upload is one the operator posted here;
/// an asset is one anybody in the Linear workspace could have posted there.
fn asset_mime(content_type: &str) -> Option<&'static str> {
    // `image/png; charset=binary` is the same type as `image/png`, and a MIME type is not
    // case-sensitive, so the parameters go and the type is folded before it is matched.
    let declared = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match declared.as_str() {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/avif" => "image/avif",
        "image/bmp" => "image/bmp",
        _ => return None,
    })
}

/// One upstream header value, cut down to what is safe to put in an error body.
///
/// What arrives here was chosen by whoever uploaded the file, so it is kept to the characters
/// a MIME type is made of and to 64 of them. Nothing that reads as markup survives, which
/// matters because the body it lands in is the one an operator will paste somewhere.
fn quoted(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "/+-._; =".contains(*c))
        .take(64)
        .collect()
}

/// What to serve one asset as: the type off the closed table, plus Linear's disposition.
///
/// The `content-security-policy` is deliberately NOT taken from Linear. It is a guard, and a
/// guard read off the same response it guards is worth nothing: whoever uploaded the file
/// picked the header too, and `default-src * 'unsafe-inline'` travels as easily as `sandbox`.
/// This origin authenticates nothing, so it states its own policy and ignores theirs.
fn asset_headers(h: &HeaderMap) -> Result<(&'static str, String), (StatusCode, String)> {
    let text = |name: header::HeaderName| {
        h.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    };
    let declared = text(header::CONTENT_TYPE).unwrap_or_default();
    let mime = asset_mime(&declared).ok_or_else(|| {
        let said = if declared.is_empty() { "nothing".to_string() } else { quoted(&declared) };
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("this route serves images only, and Linear declared that asset '{said}'"),
        )
    })?;
    // Disposition is Linear's to choose. Once the type is off the closed table the answer is an
    // image either way, so `inline` and `attachment` differ only in what the browser does with
    // a file it already refuses to run, and Linear's version carries the filename.
    let disposition = text(header::CONTENT_DISPOSITION).unwrap_or_else(|| "attachment".into());
    Ok((mime, disposition))
}

/// The asset's bytes, forwarded as they arrive, still under the cap and still on the clock.
///
/// Nothing between Linear and the browser holds the whole file. Reading it into a `Vec` first
/// measured VmHWM 17604kB to 56604kB for one oversized fetch, which is 32MB a caller with no
/// credential could pin per request. `deadline` is the same instant the key loop ran under, so
/// a body that trickles cannot outlast the request that asked for it.
fn asset_body(resp: reqwest::Response, deadline: tokio::time::Instant) -> axum::body::Body {
    let stream = futures::stream::try_unfold((resp, 0usize), move |(mut resp, seen)| async move {
        let chunk = match tokio::time::timeout_at(deadline, resp.chunk()).await {
            Err(_) => {
                return Err(std::io::Error::other(format!(
                    "the asset was still arriving after {LINEAR_ASSET_TIMEOUT_SECS}s"
                )));
            }
            Ok(Err(e)) => {
                return Err(std::io::Error::other(format!(
                    "the asset stopped arriving part way through: {e}"
                )));
            }
            Ok(Ok(None)) => return Ok(None),
            Ok(Ok(Some(chunk))) => chunk,
        };
        let seen = seen + chunk.len();
        if seen > LINEAR_ASSET_MAX_BYTES {
            return Err(std::io::Error::other(asset_too_large()));
        }
        Ok(Some((chunk, (resp, seen))))
    });
    axum::body::Body::from_stream(stream)
}

/// The one sentence for an asset past [`LINEAR_ASSET_MAX_BYTES`], said the same way whether
/// the length was declared up front or counted as it arrived.
fn asset_too_large() -> String {
    format!(
        "that asset is larger than the {}MB this route will read",
        LINEAR_ASSET_MAX_BYTES / (1024 * 1024)
    )
}

/// The `url` query parameter, and nothing else. The content type is the upstream's to
/// declare, so there is no field here for the caller to set it with.
#[derive(Deserialize)]
struct AssetQuery {
    url: String,
}

/// Every configured Linear key, in config order, deduped and without the blanks.
///
/// The route has no workspace to go on: an `assetUrl` names a file, not a team. So it tries
/// each key until one is allowed to read the file, which is what `fetch_issue_any` does with
/// the same problem for issues.
fn linear_keys(cfg: &wire::AppConfig) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for preset in &cfg.presets {
        let key = preset.linear_key.trim();
        if key.is_empty() || keys.iter().any(|k| k == key) {
            continue;
        }
        keys.push(key.to_string());
    }
    keys
}

/// The answer from the first configured key allowed to read one asset.
///
/// Separate from the handler so a test can point it at a stub, the way [`relay_request`] is:
/// the allowlist keeps the real route aimed at Linear, so the key loop is unobservable through
/// the route itself.
///
/// The body is left where it is. The handler streams it, so this returns as soon as the
/// headers are in and nothing here ever holds a file.
async fn fetch_asset(
    http: &reqwest::Client,
    url: &str,
    keys: &[String],
) -> Result<reqwest::Response, (StatusCode, String)> {
    if keys.is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            "no preset has a Linear API key configured, add one in Settings".into(),
        ));
    }

    let mut last = String::new();
    for key in keys {
        // No per-attempt deadline: the caller holds one for the whole request, keys and body
        // together, so that adding a preset cannot add 20 seconds to what a stranger can pin.
        let resp = match http.get(url).header(header::AUTHORIZATION, key).send().await {
            Ok(r) => r,
            Err(e) => {
                last = format!("the asset host was unreachable: {e}");
                continue;
            }
        };

        // Refused, not followed, for the reason the upload relay refuses one: the host check
        // ran against this URL and no other. Returned rather than tried with the next key,
        // because a redirect is not a statement about the key.
        if resp.status().is_redirection() {
            let to = redirect_target(resp.headers());
            return Err((
                StatusCode::BAD_GATEWAY,
                format!(
                    "the asset host answered HTTP {} with a redirect to {to}, and this route \
                     does not follow redirects: only the first URL is checked against the \
                     {LINEAR_ASSET_HOST} allowlist",
                    resp.status().as_u16()
                ),
            ));
        }
        if !resp.status().is_success() {
            // 401 and 403 are "not this key", which is the whole reason for the loop. Every
            // other status is remembered the same way, so what gets reported is the last
            // answer seen rather than a guess about which key should have worked.
            last = format!("Linear answered HTTP {} for that asset", resp.status().as_u16());
            continue;
        }
        return Ok(resp);
    }

    // What Linear said, and not how many keys said it: an unauthenticated caller learning the
    // preset count learns something about this deployment for the price of one bad URL.
    Err((StatusCode::BAD_GATEWAY, last))
}

/// One asset read and made into a response, under one deadline for the whole of it.
///
/// Split from the handler for the reason [`fetch_asset`] is: the allowlist keeps the route
/// itself aimed at Linear, so a stub is the only way to drive the key loop, the deadline, and
/// the size cap. `budget` is that deadline, and it is what the handler spends
/// [`LINEAR_ASSET_TIMEOUT_SECS`] on.
async fn asset_response(
    http: &reqwest::Client,
    url: &str,
    keys: &[String],
    budget: Duration,
) -> Result<Response, (StatusCode, String)> {
    // One deadline, taken once and carried into the body stream. Every key attempt and every
    // byte runs under it, so what a caller can hold is this number and never a multiple.
    let deadline = tokio::time::Instant::now() + budget;
    let slow = || {
        (
            StatusCode::GATEWAY_TIMEOUT,
            format!("Linear did not answer for that asset within {budget:?}"),
        )
    };
    let resp = tokio::time::timeout_at(deadline, fetch_asset(http, url, keys))
        .await
        .map_err(|_| slow())??;

    let (mime, disposition) = asset_headers(resp.headers())?;
    // A length past the cap is refused before a byte is read, so the common oversized case
    // costs one round trip and the caller gets a status rather than a cut-off image.
    if resp.content_length().is_some_and(|n| n > LINEAR_ASSET_MAX_BYTES as u64) {
        return Err((StatusCode::BAD_GATEWAY, asset_too_large()));
    }
    Ok((
        [
            // Off the closed table, never the string Linear sent. See [`asset_mime`].
            (header::CONTENT_TYPE, mime.to_string()),
            // A stored file is data here, never a document: this origin has no authentication
            // on any route, so a body re-read as HTML would run on it.
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            // Ours, never Linear's. See [`asset_headers`].
            (header::CONTENT_SECURITY_POLICY, "sandbox".to_string()),
            // An `assetUrl` addresses one immutable file, so a reload should not cost another
            // round trip to Linear. Private: these bytes sit behind a key.
            (header::CACHE_CONTROL, "private, max-age=3600".to_string()),
        ],
        asset_body(resp, deadline),
    )
        .into_response())
}

/// `GET /api/linear/asset?url=<assetUrl>`: read one Linear-hosted image, same-origin.
async fn linear_asset(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<AssetQuery>,
) -> Result<Response, (StatusCode, String)> {
    check_asset_target(&q.url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let keys = linear_keys(&app.config());
    asset_response(
        &app.relay_http,
        q.url.trim(),
        &keys,
        Duration::from_secs(LINEAR_ASSET_TIMEOUT_SECS),
    )
    .await
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

/// `GET /api/config` — the redacted view, each preset's Linear key included verbatim.
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
    // Account pools are replaced wholesale by this endpoint, so an omitted pool is a deletion —
    // and a clone still naming it would be stranded: `claude::rotate_once` skips a group it
    // cannot find (`continue`), so that clone freezes on whatever account it last held and is
    // never rebalanced again, with nothing logged. Repoint those clones at `auto` here, while we
    // can still see WHICH pools went away.
    heal_dangling_pool_bindings(&app, &old, &merged);
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


// --- Claude + Codex accounts ------------------------------------------------

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
async fn claude_import_check(
    State(app): State<App>,
    Json(req): Json<ImportCheckReq>,
) -> JsonResult {
    let host = clone_by_id(&app, &req.host).ok_or_else(|| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("unknown host '{}'", req.host),
        )
    })?;
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
    let host = clone_by_id(&app, &req.host).ok_or_else(|| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("unknown host '{}'", req.host),
        )
    })?;
    let res = crate::claude::import_clone_account(&app, &host)
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e))?;
    let _ = crate::claude::poll_once(&app).await;
    Ok(Json(
        json!({ "ok": true, "email": res.email, "cleared": res.cleared }),
    ))
}

/// `POST /api/claude/refresh` — force one usage poll now.
async fn claude_refresh(State(app): State<App>) -> Json<serde_json::Value> {
    Json(
        refresh_response(
            crate::claude::poll_once(&app),
            crate::claude::rotate_once(&app),
        )
        .await,
    )
}

async fn refresh_response(
    poll: impl Future<Output = anyhow::Result<bool>>,
    rotate: impl Future<Output = ()>,
) -> serde_json::Value {
    match poll.await {
        Ok(any429) => {
            rotate.await;
            json!({ "ok": true, "rateLimited": any429, "rotated": true })
        }
        Err(_) => json!({ "ok": true, "rateLimited": false, "rotated": false }),
    }
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
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("unknown host '{}'", req.host),
            )
        })?;
    if !host.managed {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("'{}' is not a managed clone", host.id),
        ));
    }
    let assignment =
        crate::claude::resolve_assignment(&app, Some(&req.account), host.claude_account_email.as_deref())
            .ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "no imported Claude accounts".into(),
                )
            })?;
    let selection = crate::claude::normalize_selection(Some(&req.account));
    let (group, email) = match assignment {
        crate::claude::Assignment::None => {
            crate::claude::clear_clone_token(&app, &host.id)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            app.claude.forget_pushed(&host.id);
            (None, None)
        }
        crate::claude::Assignment::Group { name, initial } => {
            crate::claude::push_account_to_clone(&app, &host.id, &initial)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            (Some(name), Some(initial))
        }
        crate::claude::Assignment::Account(a) => {
            crate::claude::push_account_to_clone(&app, &host.id, &a)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            (None, Some(a))
        }
        crate::claude::Assignment::AutoPending => (None, None),
    };
    let (id, email_set, group_set, sel_set) = (
        host.id.clone(),
        email.clone(),
        group.clone(),
        selection.clone(),
    );
    app.store.mutate(|s| {
        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
            h.claude_account_email = email_set;
            h.claude_group = group_set;
            h.claude_selection = Some(sel_set);
        }
    });
    Ok(Json(
        json!({ "ok": true, "account": email, "group": group, "selection": selection }),
    ))
}

/// A request naming a single imported account by email — the body for the delete endpoints.
#[derive(Deserialize)]
struct AccountRef {
    account: String,
}

/// `POST /api/claude/delete` — remove an imported Claude account by email. 400 if any clone
/// is pinned to it (the message lists them); otherwise deletes the token and reassigns
/// auto/group clones off it. Returns the ids of clones that were moved.
async fn claude_delete(State(app): State<App>, Json(req): Json<AccountRef>) -> JsonResult {
    let moved = crate::claude::delete_account(&app, req.account.trim())
        .await
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "ok": true, "moved": moved })))
}

/// `POST /api/claude/rotate` — run one group-rotation pass immediately (the rotator
/// otherwise runs every 10 min). Useful for ops + testing.
async fn claude_rotate(State(app): State<App>) -> Json<serde_json::Value> {
    crate::claude::rotate_once(&app).await;
    Json(json!({ "ok": true }))
}

// --- Codex accounts --------------------------------------------------------

#[derive(Deserialize)]
struct CodexImportReq {
    host: String,
}

/// `POST /api/codex/import/check` — confirm a clone is signed in to Codex via ChatGPT and
/// report its identity so the UI can show it before importing.
async fn codex_import_check(State(app): State<App>, Json(req): Json<CodexImportReq>) -> JsonResult {
    let host = clone_by_id(&app, &req.host).ok_or_else(|| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("unknown host '{}'", req.host),
        )
    })?;
    let auth = crate::codex::check_clone_auth(&app, &host)
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({
        "ok": true,
        "email": auth.email,
        "plan": auth.plan,
        "accountId": auth.account_id,
    })))
}

/// `POST /api/codex/import` — import a Codex account from a signed-in clone.
async fn codex_import(State(app): State<App>, Json(req): Json<CodexImportReq>) -> JsonResult {
    let host = clone_by_id(&app, &req.host).ok_or_else(|| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("unknown host '{}'", req.host),
        )
    })?;
    let res = crate::codex::import_clone_account(&app, &host)
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e))?;
    let _ = crate::codex::poll_once(&app).await;
    Ok(Json(
        json!({ "ok": true, "email": res.email, "cleared": res.cleared }),
    ))
}

/// `POST /api/codex/refresh` — force one usage poll now.
async fn codex_refresh(State(app): State<App>) -> Json<serde_json::Value> {
    Json(
        refresh_response(
            crate::codex::poll_once(&app),
            crate::codex::rotate_once(&app),
        )
        .await,
    )
}

#[derive(Deserialize)]
struct CodexSwapReq {
    host: String,
    /// Account email, `auto`, `none`, or `group:<name>`.
    account: String,
}

/// `POST /api/codex/swap` — change a clone's Codex account/group.
async fn codex_swap(
    State(app): State<App>,
    Json(req): Json<CodexSwapReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let host = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == req.host)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("unknown host '{}'", req.host),
            )
        })?;
    if !host.managed {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("'{}' is not a managed clone", host.id),
        ));
    }
    let assignment = crate::codex::resolve_assignment(&app, Some(&req.account), host.codex_account_email.as_deref())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "no imported Codex accounts".into()))?;
    let selection = crate::codex::normalize_selection(Some(&req.account));
    let (group, email) = match assignment {
        crate::codex::Assignment::None => {
            crate::codex::clear_clone_token(&app, &host.id)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            app.codex.forget_pushed(&host.id);
            (None, None)
        }
        crate::codex::Assignment::Group { name, initial } => {
            crate::codex::push_account_to_clone(&app, &host.id, &initial)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            (Some(name), Some(initial))
        }
        crate::codex::Assignment::Account(a) => {
            crate::codex::push_account_to_clone(&app, &host.id, &a)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            (None, Some(a))
        }
        crate::codex::Assignment::AutoPending => (None, None),
    };
    let (id, email_set, group_set, sel_set) = (
        host.id.clone(),
        email.clone(),
        group.clone(),
        selection.clone(),
    );
    app.store.mutate(|s| {
        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
            h.codex_account_email = email_set;
            h.codex_group = group_set;
            h.codex_selection = Some(sel_set);
        }
    });
    Ok(Json(
        json!({ "ok": true, "account": email, "group": group, "selection": selection }),
    ))
}

/// `POST /api/codex/delete` — remove an imported Codex account by email (the Codex twin of
/// [`claude_delete`]). 400 if any clone is pinned to it; otherwise deletes + reassigns.
async fn codex_delete(State(app): State<App>, Json(req): Json<AccountRef>) -> JsonResult {
    let moved = crate::codex::delete_account(&app, req.account.trim())
        .await
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "ok": true, "moved": moved })))
}

/// `POST /api/codex/rotate` — run one Codex group-rotation pass immediately.
async fn codex_rotate(State(app): State<App>) -> Json<serde_json::Value> {
    crate::codex::rotate_once(&app).await;
    Json(json!({ "ok": true }))
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

    fn column(id: &str, clone_ids: &[&str]) -> wire::BoardColumn {
        wire::BoardColumn {
            id: id.into(),
            title: id.into(),
            clone_ids: clone_ids.iter().map(|s| (*s).to_string()).collect(),
            archive: false,
        }
    }

    #[tokio::test]
    async fn board_columns_are_replaced_wholesale_and_persist() {
        let app = App::test_app();

        board_put(State(app.clone()), Json(BoardPutReq { columns: vec![column("todo", &["a"])] }))
            .await;
        // A second write is a replacement, not a merge: the operator deleting a column has to
        // be able to make the board smaller.
        let after =
            board_put(State(app.clone()), Json(BoardPutReq { columns: vec![column("doing", &[])] }))
                .await;

        assert_eq!(after.0.board_columns, vec![column("doing", &[])]);
        assert_eq!(app.store.get().board_columns, vec![column("doing", &[])]);
    }

    #[tokio::test]
    async fn an_empty_column_list_clears_the_board() {
        let app = App::test_app();
        board_put(State(app.clone()), Json(BoardPutReq { columns: vec![column("todo", &[])] }))
            .await;

        board_put(State(app.clone()), Json(BoardPutReq { columns: Vec::new() })).await;

        // Deleting the last column is legal; the frontend falls back to a default column so
        // no clone is ever left without one.
        assert!(app.store.get().board_columns.is_empty());
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


    // --- POST /api/clone `hostname` mode (raw clone, fleet CLI) ---

    #[tokio::test]
    async fn clone_hostname_mode_registers_clone_op() {
        let app = test_app();
        let body = json!({
            "image": "tmpl:latest",
            "hostname": "w-mod-claude",
            "claudeAccount": "auto",
        });
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

    /// Deleting a pool must not strand the clones bound to it.
    ///
    /// Pools are replaced wholesale by `PUT /api/config`, so an omitted pool is a deletion.
    /// `claude::rotate_once` skips a group it cannot find, so a clone left naming a deleted pool
    /// is never rebalanced again — it freezes on its last account, silently, forever.
    #[test]
    fn deleting_a_pool_repoints_its_clones_at_auto() {
        let app = test_app();
        let pool = |n: &str| wire::CloneGroup { name: n.into(), accounts: vec![] };
        let old = wire::AppConfig {
            clone_groups: vec![pool("keep"), pool("doomed")],
            codex_groups: vec![pool("gpt")],
            ..Default::default()
        };
        app.store.mutate(|s| {
            s.hosts = vec![
                wire::RmngClone {
                    id: "bound".into(),
                    claude_group: Some("doomed".into()),
                    claude_selection: Some("group:doomed".into()),
                    ..Default::default()
                },
                wire::RmngClone {
                    id: "survivor".into(),
                    claude_group: Some("keep".into()),
                    claude_selection: Some("group:keep".into()),
                    ..Default::default()
                },
                wire::RmngClone {
                    id: "pinned".into(),
                    claude_selection: Some("me@x.com".into()),
                    claude_account_email: Some("me@x.com".into()),
                    ..Default::default()
                },
                wire::RmngClone {
                    id: "codex-bound".into(),
                    codex_group: Some("gpt".into()),
                    codex_selection: Some("group:gpt".into()),
                    ..Default::default()
                },
            ];
        });

        // Drop `doomed` (Claude) and `gpt` (Codex); keep `keep`.
        let merged = wire::AppConfig {
            clone_groups: vec![pool("keep")],
            codex_groups: vec![],
            ..Default::default()
        };
        heal_dangling_pool_bindings(&app, &old, &merged);

        let by_id = |id: &str| app.store.get().hosts.into_iter().find(|h| h.id == id).unwrap();
        // The stranded clone keeps working, on `auto`, and no longer names a pool that is gone.
        let bound = by_id("bound");
        assert_eq!(bound.claude_selection.as_deref(), Some("auto"));
        assert_eq!(bound.claude_group, None);
        // A clone on a surviving pool is untouched — healing must be scoped to what was deleted.
        let survivor = by_id("survivor");
        assert_eq!(survivor.claude_selection.as_deref(), Some("group:keep"));
        assert_eq!(survivor.claude_group.as_deref(), Some("keep"));
        // A pinned clone is not a pool clone; an explicit pin is the operator's choice to keep.
        let pinned = by_id("pinned");
        assert_eq!(pinned.claude_selection.as_deref(), Some("me@x.com"));
        // Providers heal independently.
        let cx = by_id("codex-bound");
        assert_eq!(cx.codex_selection.as_deref(), Some("auto"));
        assert_eq!(cx.codex_group, None);
        // ...and the Claude side of that same clone was never bound, so it stays unset.
        assert_eq!(cx.claude_selection, None);
    }

    /// A typo'd pool name must be a 400, not a silently tokenless clone.
    ///
    /// `claude::pick_group_account` returns `None` for an unknown pool, which cascades into
    /// `resolve_assignment` returning `None` and `jobs::run_clone` skipping the account step
    /// entirely — a running clone with no token and nothing in the log. The boundary check is
    /// the only place this can still fail usefully.
    #[tokio::test]
    async fn clone_rejects_an_unknown_account_pool() {
        let app = test_app();
        *app.cfg.write().unwrap() = wire::AppConfig {
            clone_groups: vec![wire::CloneGroup { name: "pooled".into(), accounts: vec![] }],
            codex_groups: vec![wire::CloneGroup { name: "gpt".into(), accounts: vec![] }],
            ..app.config()
        };
        let create = |body: serde_json::Value| {
            clone(State(app.clone()), HeaderMap::new(), Json(body))
        };

        // A configured pool is accepted (reaches the op, i.e. past validation).
        let ok = create(json!({
            "image": "tmpl:latest", "hostname": "w-ok", "claudeAccount": "group:pooled",
        }))
        .await;
        assert!(ok.is_ok(), "a configured pool must pass validation");

        // A typo is a 400 naming what IS configured, per provider.
        let err = create(json!({
            "image": "tmpl:latest", "hostname": "w-bad", "claudeAccount": "group:pooed",
        }))
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("pooed") && err.1.contains("pooled"), "unhelpful: {}", err.1);

        // The two providers have independent pool lists — a Claude pool is not a Codex pool.
        let err = create(json!({
            "image": "tmpl:latest", "hostname": "w-x", "codexAccount": "group:pooled",
        }))
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Non-pool selections are untouched: an email may legitimately not be imported yet.
        // Distinct hostnames — a repeat would trip the duplicate-name check, not the pool check.
        for (i, sel) in ["auto", "none", "nobody@example.com"].iter().enumerate() {
            let r = create(json!({
                "image": "tmpl:latest",
                "hostname": format!("w-sel-{i}"),
                "claudeAccount": sel,
            }))
            .await;
            assert!(r.is_ok(), "selection {sel:?} must not be rejected");
        }
    }

    /// The preset default is step 3 of the chain — weaker than an explicit request and weaker
    /// than a parent's inherited selection, stronger than nothing.
    #[test]
    fn preset_account_default_fills_the_gap_but_never_outranks() {
        let presets = vec![wire::Preset {
            name: "backend".into(),
            claude_account: "group:pooled".into(),
            codex_account: "gpt@team.com".into(),
            ..Default::default()
        }];
        let p = Some(&presets[0]);

        // No request, no parent → the preset decides, per provider.
        let (c, x, _) = effective_accounts_preset(None, false, None, false, None, true, p, &presets);
        assert_eq!(c.as_deref(), Some("group:pooled"));
        assert_eq!(x.as_deref(), Some("gpt@team.com"));

        // An explicit request outranks it — including an explicit `none`, which is a real
        // choice (boot tokenless) and must not fall through to the preset.
        let (c, x, _) = effective_accounts_preset(
            None,
            true,
            Some("me@x.com".into()),
            true,
            None,
            true,
            p,
            &presets,
        );
        assert_eq!(c.as_deref(), Some("me@x.com"));
        assert_eq!(x, None, "an explicit clear must not be back-filled by the preset");

        // A parent's selection outranks it too (sub clones follow their parent, not the preset).
        let parent = wire::RmngClone {
            id: "p".into(),
            claude_selection: Some("auto".into()),
            ..Default::default()
        };
        let (c, x, _) =
            effective_accounts_preset(Some(&parent), false, None, false, None, true, p, &presets);
        assert_eq!(c.as_deref(), Some("auto"), "the parent wins over the preset");
        // ...but only for the provider the parent actually had. Codex still falls to the preset.
        assert_eq!(x.as_deref(), Some("gpt@team.com"));
    }

    /// A BLANK preset default means "no opinion" and must fall through, NOT be treated as an
    /// empty selection. Confusing the two would silently bind every clone of an unconfigured
    /// preset to an empty string instead of letting the account layer pick.
    #[test]
    fn a_blank_preset_default_is_no_opinion() {
        let presets = vec![wire::Preset { name: "bare".into(), ..Default::default() }];
        let (c, x, _) = effective_accounts_preset(
            None,
            false,
            None,
            false,
            None,
            true,
            Some(&presets[0]),
            &presets,
        );
        assert_eq!(c, None);
        assert_eq!(x, None);
        // Whitespace is not an opinion either.
        let ws = vec![wire::Preset {
            name: "ws".into(),
            claude_account: "   ".into(),
            ..Default::default()
        }];
        let (c, _, _) =
            effective_accounts_preset(None, false, None, false, None, true, Some(&ws[0]), &ws);
        assert_eq!(c, None);
    }

    /// The plain / ticket create modes have no parent, so they use the shorter helper — which
    /// must agree with the chain above on the two rules that matter.
    #[test]
    fn account_or_preset_default_matches_the_chain() {
        let preset = wire::Preset {
            name: "p".into(),
            claude_account: "group:pooled".into(),
            ..Default::default()
        };
        let req = "me@x.com".to_string();
        // Request wins.
        assert_eq!(
            account_or_preset_default(Some(&req), Some(&preset), |p| &p.claude_account).as_deref(),
            Some("me@x.com")
        );
        // Absent request → the preset's default.
        assert_eq!(
            account_or_preset_default(None, Some(&preset), |p| &p.claude_account).as_deref(),
            Some("group:pooled")
        );
        // Blank preset default, and no preset at all, both fall through.
        let bare = wire::Preset { name: "bare".into(), ..Default::default() };
        assert_eq!(account_or_preset_default(None, Some(&bare), |p| &p.claude_account), None);
        assert_eq!(account_or_preset_default(None, None, |p| &p.claude_account), None);
    }

    #[test]
    fn sub_clone_inherits_accounts_and_preset_unless_overridden() {
        let presets = vec![
            wire::Preset { name: "parent-preset".into(), ..Default::default() },
            wire::Preset { name: "override-preset".into(), ..Default::default() },
        ];
        // The parent is on `auto` and has LANDED on a concrete account. What a sub clone
        // inherits is the *selection*, not the resolved email — otherwise a child asking for
        // `auto` would be silently pinned to whatever its parent happens to be running.
        let parent = wire::RmngClone {
            id: "p".into(),
            managed: true,
            claude_selection: Some("auto".into()),
            claude_account_email: Some("landed@x.com".into()),
            codex_selection: Some("group:gpt".into()),
            preset_name: Some("parent-preset".into()),
            ..Default::default()
        };
        let name = |p: Option<&wire::Preset>| p.map(|p| p.name.clone());

        // Nothing specified → inherit all three from the parent.
        let (c, x, pr) =
            effective_accounts_preset(Some(&parent), false, None, false, None, false, None, &presets);
        assert_eq!(c, Some("auto".into()), "the selection is inherited, not the resolved email");
        assert_eq!(x, Some("group:gpt".into()));
        assert_eq!(name(pr), Some("parent-preset".into()));

        // Explicit values override inheritance, per provider independently.
        let (c, x, pr) = effective_accounts_preset(
            Some(&parent),
            true,
            Some("me@x.com".into()),
            false,
            None,
            true,
            Some(&presets[1]),
            &presets,
        );
        assert_eq!(c, Some("me@x.com".into()));
        assert_eq!(x, Some("group:gpt".into()), "an unspecified provider still inherits");
        assert_eq!(name(pr), Some("override-preset".into()));

        // Explicit `none` (specified, but resolving to None) opts out of inheritance.
        let (c, x, pr) =
            effective_accounts_preset(Some(&parent), true, None, true, None, true, None, &presets);
        assert_eq!(c, None);
        assert_eq!(x, None);
        assert_eq!(pr, None);

        // No parent → no inheritance.
        let (c, x, pr) =
            effective_accounts_preset(None, false, None, false, None, false, None, &presets);
        assert_eq!(c, None);
        assert_eq!(x, None);
        assert!(pr.is_none());

        // Parent names a preset that no longer exists → gracefully no preset.
        let orphan = wire::RmngClone { preset_name: Some("gone".into()), ..parent.clone() };
        let (_c, _x, pr) =
            effective_accounts_preset(Some(&orphan), false, None, false, None, false, None, &presets);
        assert!(pr.is_none());
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
        let key_p = app.clone_keys.mint("p");
        assert_eq!(
            resolve_parent(&app, &json!({}), &header(&key_p)).unwrap(),
            Some("p".into())
        );
        // A sub-clone caller can't nest deeper (one level) → top-level.
        let key_c = app.clone_keys.mint("c");
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

    /// End-to-end through the real router: the ticket column saves its order with `PUT` and
    /// the `{ ticketIds }` envelope, and the patched state comes straight back.
    ///
    /// Two things this pins that a direct handler call could not. First the route itself:
    /// `/api/tickets/:id` is also a `PUT`, so `order` would be a perfectly good `:id` if the
    /// matcher preferred the parameter, and the save would silently try to write a Linear
    /// issue called "order". Second the reload path: a second `GET /api/state` proves the
    /// order is in the state document and not just in the response body.
    #[tokio::test]
    async fn ticket_order_put_round_trips_over_http() {
        let app = test_app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        let base = format!("http://{addr}");
        let http = reqwest::Client::new();

        // Mixed case in, lowercase out: the browser ranks by lowercased id, so that is the
        // only case worth storing.
        let put = http
            .put(format!("{base}/api/tickets/order"))
            .json(&serde_json::json!({ "ticketIds": ["WE-142", "dev-7", "We-9"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(put.status(), reqwest::StatusCode::OK);
        let patched: wire::ControlState = put.json().await.unwrap();
        assert_eq!(patched.ticket_order, vec!["we-142", "dev-7", "we-9"]);

        // ...and it survives into the state a reloading page reads.
        let state: wire::ControlState = http
            .get(format!("{base}/api/state"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(state.ticket_order, vec!["we-142", "dev-7", "we-9"]);

        // An empty list is a real value (the operator cleared the arrangement), not a no-op.
        let cleared: wire::ControlState = http
            .put(format!("{base}/api/tickets/order"))
            .json(&serde_json::json!({ "ticketIds": [] }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(cleared.ticket_order.is_empty());
    }

    // --- POST /api/linear/upload-relay ---
    //
    // The route exists because a page cannot PUT to Linear's signed bucket URL. It holds no
    // key and never calls Linear, so what is worth pinning is narrow: what it will send bytes
    // to, and whether the headers arrive as they left.

    #[test]
    fn the_relay_puts_only_to_the_signed_upload_bucket() {
        assert!(
            check_relay_target(
                "https://storage.googleapis.com/uploads/abc?X-Goog-Expires=60&X-Goog-Signature=de"
            )
            .is_ok()
        );

        // A name that merely contains the allowed one is a different host. This is the shape
        // an attacker reaches for first, and a `contains` check would pass it.
        for url in [
            "https://storage.googleapis.com.example.net/x",
            "https://evil.storage.googleapis.com.co/x",
            "https://169.254.169.254/latest/meta-data/",
            "http://storage.googleapis.com/x",
            "https://storage.googleapis.com:8443/x",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(check_relay_target(url).is_err(), "should refuse {url}");
        }
    }

    #[test]
    fn signed_headers_are_forwarded_verbatim_and_hop_headers_are_not() {
        let parsed = relay_headers(
            r#"[{"key":"content-type","value":"image/png"},
                {"key":"x-goog-content-length-range","value":"0,7"},
                {"key":"Host","value":"elsewhere.example"},
                {"key":"content-length","value":"999"}]"#,
        )
        .unwrap();

        // The two the signature covers survive exactly. The two that describe this hop do not.
        assert_eq!(
            parsed,
            vec![
                ("content-type".to_string(), "image/png".to_string()),
                ("x-goog-content-length-range".to_string(), "0,7".to_string()),
            ]
        );
    }

    #[test]
    fn a_malformed_header_entry_is_refused_rather_than_dropped() {
        assert!(relay_headers("[{\"key\":\"content-type\"}]").is_err());
        assert!(relay_headers("[{\"key\":\"\",\"value\":\"x\"}]").is_err());
        assert!(relay_headers("{\"content-type\":\"image/png\"}").is_err());
        assert!(relay_headers("nonsense").is_err());
        assert!(relay_headers("[]").unwrap().is_empty());
    }

    #[test]
    fn a_bucket_refusal_names_the_reason_it_gave() {
        let expired = relay_failure(
            400,
            "<?xml version='1.0'?><Error><Code>ExpiredToken</Code><Message>…</Message></Error>",
        );
        assert!(expired.contains("expired"), "{expired}");
        assert!(expired.contains("60 seconds"), "{expired}");

        let signature = relay_failure(403, "<Error><Code>SignatureDoesNotMatch</Code></Error>");
        assert!(signature.contains("SignatureDoesNotMatch"), "{signature}");

        // A body with no XML at all still names the status rather than saying nothing.
        assert!(relay_failure(500, "").contains("500"));
    }

    /// One multipart body, in the shape the browser's `FormData` produces. reqwest is built
    /// without its `multipart` feature here, and hand-rolling it also pins the exact field
    /// names the frontend sends.
    fn multipart(text: &[(&str, &str)], file: Option<(&str, &[u8])>) -> (String, Vec<u8>) {
        let boundary = "----rmngtestboundary";
        let mut body: Vec<u8> = Vec::new();
        for (name, value) in text {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
        if let Some((filename, bytes)) = file {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
                     filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    /// End-to-end through the real router: the route is reachable at the address the frontend
    /// posts to, it parses the browser's three fields, and it refuses a target that is not the
    /// signed-upload bucket. That refusal is the only thing standing between an unauthenticated
    /// LAN port and a general outbound proxy, so it is asserted over a socket and not in
    /// isolation.
    #[tokio::test]
    async fn upload_relay_refuses_a_host_that_is_not_the_bucket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(test_app())).await.unwrap() });
        let http = reqwest::Client::new();

        let post = |url: &'static str| {
            let http = http.clone();
            let base = format!("http://{addr}/api/linear/upload-relay");
            async move {
                let (ct, body) = multipart(
                    &[
                        ("url", url),
                        ("headers", r#"[{"key":"content-type","value":"image/png"}]"#),
                    ],
                    Some(("shot.png", b"\x89PNG\r\n\x1a\n")),
                );
                let resp = http
                    .post(base)
                    .header("content-type", ct)
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                (resp.status(), resp.text().await.unwrap())
            }
        };

        // A name that merely contains the allowed one, and a LAN address the operator's own
        // network would answer. Both name the one host the route will talk to.
        for url in [
            "https://storage.googleapis.com.example.net/steal",
            "https://127.0.0.1/steal",
        ] {
            let (status, text) = post(url).await;
            assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "for {url}");
            assert!(text.contains(UPLOAD_RELAY_HOST), "for {url}: {text}");
        }

        // The right host over the wrong scheme is still refused, and says which rule caught it.
        let (status, text) = post("http://storage.googleapis.com/steal").await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains("https"), "{text}");
    }

    /// The bytes and every header reach the target unchanged. The allowlist keeps a real PUT
    /// pointed at Google, so this drives `relay_request` directly against a stub that answers
    /// with what it received, which is the only way to observe the forwarding at all.
    #[tokio::test]
    async fn the_relay_sends_the_declared_content_type_and_the_size_range_it_was_given() {
        use axum::extract::Request;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stub = Router::new().route(
            "/put",
            put(|req: Request| async move {
                let seen: Vec<String> = req
                    .headers()
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.to_str().unwrap_or("")))
                    .collect();
                let body = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap();
                Json(json!({ "headers": seen, "len": body.len() }))
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let headers =
            relay_headers(r#"[{"key":"content-type","value":"image/png"},{"key":"x-goog-content-length-range","value":"0,8"}]"#)
                .unwrap();
        let bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let seen: serde_json::Value =
            relay_request(&reqwest::Client::new(), &format!("http://{addr}/put"), &headers)
                .body(bytes.clone())
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

        let lines = seen["headers"].as_array().unwrap();
        let has = |want: &str| lines.iter().any(|l| l.as_str() == Some(want));
        // Exactly what was declared, not a substitute: `application/octet-stream` here is a
        // 403 SignatureDoesNotMatch from the real bucket.
        assert!(has("content-type: image/png"), "{lines:?}");
        // Exactly the range that was signed: one byte outside it is a 400 EntityTooLarge.
        assert!(has("x-goog-content-length-range: 0,8"), "{lines:?}");
        assert_eq!(seen["len"].as_u64(), Some(bytes.len() as u64));
    }

    /// A 30x from the checked host is a dead end, not a second request.
    ///
    /// `check_relay_target` reads the URL the caller sent and no other, so a redirect followed
    /// would replay the PUT, its body, and its signed headers at a host, scheme, and port
    /// nothing looked at, on a port with no authentication on any route. Google's bucket does
    /// not answer with a `Location` today, which is a fact about somebody else's server. This
    /// pins the property on ours.
    #[tokio::test]
    async fn a_redirect_from_the_upload_target_is_refused_rather_than_followed() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Where a followed redirect would land. It counts what actually arrives.
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let victim = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let victim_addr = victim.local_addr().unwrap();
        let victim_app = Router::new().route(
            "/steal",
            put(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    "pwnd"
                }
            }),
        );
        tokio::spawn(async move { axum::serve(victim, victim_app).await.unwrap() });

        // The checked host's stand-in: it answers with 307, which is the one that preserves
        // the method and the body.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let to = format!("http://{victim_addr}/steal");
        let stub = Router::new().route(
            "/put",
            put(move || {
                let to = to.clone();
                async move { (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, to)]) }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let headers = relay_headers(r#"[{"key":"content-type","value":"image/png"}]"#).unwrap();
        let png = b"\x89PNG\r\n\x1a\n".to_vec();
        let target = format!("http://{addr}/put");

        // The client the route uses: the redirect comes back as the answer.
        let refused = relay_request(&app.relay_http, &target, &headers)
            .body(png.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status().as_u16(), 307, "the 30x itself must be the response");
        assert_eq!(hits.load(Ordering::SeqCst), 0, "the PUT must not have been replayed");

        // The shared client still follows, which is what makes the policy the difference and
        // not some property of the stub. Other callers depend on this one following.
        let followed = relay_request(&app.http, &target, &headers)
            .body(png)
            .send()
            .await
            .unwrap();
        assert_eq!(followed.status().as_u16(), 200);
        assert_eq!(followed.text().await.unwrap(), "pwnd");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // And the sentence the operator reads says which rule caught it.
        let said = relay_redirect_refused(302, "https://evil.example/steal");
        assert!(said.contains("302") && said.contains("evil.example"), "{said}");
        assert!(said.contains(UPLOAD_RELAY_HOST), "{said}");
    }

    /// `file` first, then `url`. The frontend never sends this order. A caller that does is
    /// the reason the pre-buffer cap exists.
    fn multipart_file_first(url: &str, bytes: &[u8]) -> (String, Vec<u8>) {
        let boundary = "----rmngtestboundary";
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
                 filename=\"big.png\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"url\"\r\n\r\n{url}\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    /// The target is decided before the bytes are held, and what is held while no target has
    /// been decided is bounded.
    ///
    /// Four concurrent 60MB posts at a host this refuses used to take the process from 35MB to
    /// 212MB resident, on a route with no authentication and no concurrency cap. The three
    /// cases below are the three orders a client can send, told apart by which sentence comes
    /// back: the host message can only come from a check that ran, and the ordering message
    /// can only come from the cap.
    #[tokio::test]
    async fn the_upload_target_is_checked_before_the_file_is_buffered() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(test_app())).await.unwrap() });
        let http = reqwest::Client::new();
        let url = format!("http://{addr}/api/linear/upload-relay");
        let send = |ct: String, body: Vec<u8>| {
            let (http, url) = (http.clone(), url.clone());
            async move {
                let resp =
                    http.post(url).header("content-type", ct).body(body).send().await.unwrap();
                (resp.status(), resp.text().await.unwrap())
            }
        };

        // `url` first and refused: the file field is never reached at all, so its size is
        // irrelevant. One byte is enough to prove which check answered.
        let (ct, body) = multipart(
            &[("url", "https://storage.googleapis.com.example.net/steal")],
            Some(("shot.png", b"\x89PNG\r\n\x1a\n")),
        );
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains(UPLOAD_RELAY_HOST), "{text}");

        // `url` last and small: buffered, then checked. Out of order is handled, not refused
        // for being out of order. The answer names the host, not the field order.
        let (ct, body) =
            multipart_file_first("https://127.0.0.1/steal", b"\x89PNG\r\n\x1a\n");
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains(UPLOAD_RELAY_HOST), "{text}");

        // `url` last and past the cap: refused for the order, well inside the 64MB body limit
        // that would otherwise be the only bound.
        let big = vec![0u8; UPLOAD_RELAY_PREBUFFER + 64 * 1024];
        let (ct, body) = multipart_file_first("https://storage.googleapis.com/ok", &big);
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains("'url' field before the 'file' field"), "{text}");
    }

    // --- GET /api/linear/asset ---
    //
    // The read half. A Linear `assetUrl` answers an unauthenticated GET with 401 and no
    // redirect, and `uploads.linear.app` leaves `authorization` out of its CORS allow-list, so
    // neither an `<img>` nor a `fetch` in the page can read one. This route holds a key and
    // hands the bytes back same-origin.

    #[test]
    fn the_asset_proxy_reads_only_from_linears_asset_host() {
        assert!(check_asset_target("https://uploads.linear.app/ab/cd/shot.png").is_ok());

        // Userinfo does not move the host, and case and IDN are folded by the same parser
        // that picks the connection, so all three of these ARE the allowed host.
        assert!(check_asset_target("https://evil.tld@uploads.linear.app/x").is_ok());
        assert!(check_asset_target("https://UPLOADS.LINEAR.APP/x").is_ok());
        assert!(check_asset_target("https://uploads.linear.app:443/x").is_ok());

        for url in [
            // The other route's allowed host. Two allowlists, one host each, on purpose.
            "https://storage.googleapis.com/x",
            "https://uploads.linear.app.example.net/x",
            "https://uploads.linear.app%2eevil.tld/x",
            "https://uploads.linear.app./x",
            "http://uploads.linear.app/x",
            "https://uploads.linear.app:8443/x",
            "https://169.254.169.254/latest/meta-data/",
            "https://[::1]/x",
            "file:///etc/passwd",
            "not a url",
            "",
        ] {
            assert!(check_asset_target(url).is_err(), "should refuse {url}");
        }
    }

    /// Over the wire, at the address the browser puts in an `<img src>`. The allowlist is the
    /// only thing between an unauthenticated LAN port and a read proxy for the whole internet,
    /// so it is asserted through the router and not just in isolation.
    #[tokio::test]
    async fn the_asset_route_refuses_a_host_that_is_not_linears() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(test_app())).await.unwrap() });
        let http = reqwest::Client::new();
        let base = format!("http://{addr}/api/linear/asset");

        for url in ["https://evil.example/x", "https://169.254.169.254/latest/meta-data/"] {
            let resp = http.get(&base).query(&[("url", url)]).send().await.unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "for {url}");
            assert!(resp.text().await.unwrap().contains(LINEAR_ASSET_HOST), "for {url}");
        }

        // No `url` at all is a bad request too, rather than a fetch of something empty.
        let resp = http.get(&base).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    /// An `assetUrl` names a file, not a team, so there is no workspace to pick a key from.
    /// Each configured key is tried until one is allowed to read it, the way `fetch_issue_any`
    /// answers the same question for issues. The allowlist keeps the real route pointed at
    /// Linear, so this drives `fetch_asset` against a stub, as the relay's forwarding test does.
    #[tokio::test]
    async fn the_asset_proxy_tries_every_configured_key_until_one_can_read_the_file() {
        use axum::extract::Request;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let png = b"\x89PNG\r\n\x1a\nrmng".to_vec();
        let served = png.clone();
        let tries = Arc::new(AtomicUsize::new(0));
        let counter = tries.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stub = Router::new().route(
            "/asset",
            get(move |req: Request| {
                let (served, counter) = (served.clone(), counter.clone());
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let key = req
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if key != "second" {
                        return (StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "application/json")], b"{\"error\":\"unauthorized\"}".to_vec());
                    }
                    (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], served)
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let url = format!("http://{addr}/asset");

        // The second key is the one with access. The bytes and the upstream's own content
        // type come back, and both keys were spent getting there.
        let keys = vec!["first".to_string(), "second".to_string()];
        let resp = fetch_asset(&app.relay_http, &url, &keys).await.unwrap();
        let (mime, _) = asset_headers(resp.headers()).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(resp.bytes().await.unwrap().to_vec(), png);
        assert_eq!(tries.load(Ordering::SeqCst), 2);

        // No key has access: the answer reports what Linear said, and only that. How many
        // presets this deployment holds is not something a caller with no credential is told.
        let (status, text) =
            fetch_asset(&app.relay_http, &url, &["first".into(), "third".into()]).await.unwrap_err();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(text.contains("401"), "{text}");
        assert!(!text.contains("key(s)") && !text.contains("2 key"), "{text}");

        // No key configured at all says so, instead of reporting a network failure.
        let (_, text) = fetch_asset(&app.relay_http, &url, &[]).await.unwrap_err();
        assert!(text.contains("no preset has a Linear API key"), "{text}");
    }

    /// Both Linear routes, against Linear, in the order the browser drives them.
    ///
    /// Everything above stubs the far side, which is the only way to assert the forwarding and
    /// the key loop at all. What no stub can answer is whether the pair still fits Linear's
    /// own behaviour: whether the signed PUT is accepted as replayed, and whether the
    /// `assetUrl` it produces is readable with the key that produced it. So this one is real,
    /// and `#[ignore]`d for it.
    ///
    ///     LINEAR_API_KEY=… cargo test -p control-server \
    ///       the_two_linear_routes_round_trip_an_image -- --ignored --nocapture
    ///
    /// It uploads one 70-byte PNG and touches no issue.
    #[tokio::test]
    #[ignore = "talks to api.linear.app and uploads.linear.app, needs LINEAR_API_KEY"]
    async fn the_two_linear_routes_round_trip_an_image() {
        let key = std::env::var("LINEAR_API_KEY").expect("set LINEAR_API_KEY to run this");
        // 1x1 RGBA, the smallest thing that is honestly a PNG.
        let png = B64
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
            .unwrap();

        let app = test_app();
        *app.cfg.write().unwrap() = wire::AppConfig {
            presets: vec![
                // A key with no access leads, so the asset route has to fall past it.
                wire::Preset { name: "dud".into(), linear_key: "lin_api_not_a_key".into(), ..Default::default() },
                wire::Preset { name: "real".into(), linear_key: key.clone(), ..Default::default() },
            ],
            ..app.config()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        let http = reqwest::Client::new();

        // The mutation the BROWSER runs. Never this server's, which is the whole point of the
        // relay: it is handed a signed URL and replays one PUT at it.
        let signed: serde_json::Value = http
            .post("https://api.linear.app/graphql")
            .header("authorization", &key)
            .json(&json!({
                "query": "mutation($contentType: String!, $filename: String!, $size: Int!) { \
                            fileUpload(contentType: $contentType, filename: $filename, size: $size) { \
                              success uploadFile { uploadUrl assetUrl headers { key value } } } }",
                "variables": { "contentType": "image/png", "filename": "rmng-proxy-probe.png", "size": png.len() },
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let file = signed.pointer("/data/fileUpload/uploadFile").expect("fileUpload refused");
        let upload_url = file["uploadUrl"].as_str().unwrap().to_string();
        let asset_url = file["assetUrl"].as_str().unwrap().to_string();
        // `content-type` leads, as `putHeaders` builds it: it is signed but not listed.
        let mut headers = vec![json!({ "key": "content-type", "value": "image/png" })];
        headers.extend(file["headers"].as_array().unwrap().iter().cloned());
        println!("assetUrl {asset_url}");

        let headers_raw = serde_json::to_string(&headers).unwrap();
        let (ct, body) = multipart(
            &[("url", upload_url.as_str()), ("headers", headers_raw.as_str())],
            Some(("rmng-proxy-probe.png", &png)),
        );
        let put = http
            .post(format!("http://{addr}/api/linear/upload-relay"))
            .header("content-type", ct)
            .body(body)
            .send()
            .await
            .unwrap();
        let put_status = put.status();
        println!("relay -> {} {}", put_status, put.text().await.unwrap());
        assert!(put_status.is_success());

        // The `<img src>` an editor in the page would use. Unauthenticated, same-origin.
        let got = http
            .get(format!("http://{addr}/api/linear/asset"))
            .query(&[("url", &asset_url)])
            .send()
            .await
            .unwrap();
        assert_eq!(got.status(), reqwest::StatusCode::OK);
        let seen_ct = got.headers()[header::CONTENT_TYPE].to_str().unwrap().to_string();
        for (name, value) in got.headers() {
            println!("asset header {name}: {}", value.to_str().unwrap_or("?"));
        }
        let bytes = got.bytes().await.unwrap().to_vec();
        println!("asset -> 200 {seen_ct} ({} bytes)", bytes.len());
        assert_eq!(seen_ct, "image/png");
        assert_eq!(bytes, png, "the bytes read back must be the bytes uploaded");

        // And the same URL with nothing but the host changed is refused.
        let forged = asset_url.replace(LINEAR_ASSET_HOST, "uploads.linear.app.example.net");
        let refused = http
            .get(format!("http://{addr}/api/linear/asset"))
            .query(&[("url", &forged)])
            .send()
            .await
            .unwrap();
        let status = refused.status();
        let text = refused.text().await.unwrap();
        println!("forged host -> {status} {text}");
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains(LINEAR_ASSET_HOST));
    }

    /// The stored HTML asset, through the real route, with a real key.
    ///
    /// Linear serves it as `text/html` because that is the `contentType` its uploader
    /// declared, and forwarding that put `<script>alert(document.domain)</script>` on this
    /// origin. The upstream answer is printed alongside ours, so the two can be read together.
    ///
    ///     LINEAR_API_KEY=… cargo test -p control-server \
    ///       a_stored_html_asset_is_refused_by_the_live_route -- --ignored --nocapture
    ///
    /// It reads one file and touches no issue.
    #[tokio::test]
    #[ignore = "reads one asset from uploads.linear.app, needs LINEAR_API_KEY"]
    async fn a_stored_html_asset_is_refused_by_the_live_route() {
        let key = std::env::var("LINEAR_API_KEY").expect("set LINEAR_API_KEY to run this");
        let payload = "https://uploads.linear.app/d41ca53e-9014-4d04-b567-5eb5a10a5ac9\
                       /3b114521-5436-4225-b9b8-d6ae5533b21e\
                       /4f5dba7e-180d-41d7-aa02-2ad519a89c08";

        let app = test_app();
        *app.cfg.write().unwrap() = wire::AppConfig {
            presets: vec![wire::Preset {
                name: "real".into(),
                linear_key: key.clone(),
                ..Default::default()
            }],
            ..app.config()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        let http = reqwest::Client::new();

        // What Linear itself answers, so the two sides of the boundary are on one page.
        let up = http.get(payload).header("authorization", &key).send().await.unwrap();
        println!("UPSTREAM status={}", up.status().as_u16());
        for name in ["content-type", "content-disposition", "content-security-policy", "x-content-type-options"] {
            if let Some(v) = up.headers().get(name) {
                println!("UPSTREAM header {name}: {}", v.to_str().unwrap_or("?"));
            }
        }
        println!("UPSTREAM body: {}", up.text().await.unwrap_or_default().trim());

        let got = http
            .get(format!("http://{addr}/api/linear/asset"))
            .query(&[("url", payload)])
            .send()
            .await
            .unwrap();
        let status = got.status();
        println!("PROXY status={}", status.as_u16());
        for (name, value) in got.headers() {
            println!("PROXY header {name}: {}", value.to_str().unwrap_or("?"));
        }
        let body = got.text().await.unwrap_or_default();
        println!("PROXY body: {}", body.trim());

        assert_eq!(status, reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(!body.contains("<script>"), "the payload must not reach this origin: {body}");
        assert!(body.contains("images only"), "{body}");
    }

    #[test]
    fn the_asset_proxy_takes_every_distinct_configured_key_in_config_order() {
        let preset = |key: &str| wire::Preset { linear_key: key.into(), ..Default::default() };
        let cfg = wire::AppConfig {
            presets: vec![preset(""), preset("K2"), preset("K1"), preset("K2"), preset("  ")],
            ..Default::default()
        };
        assert_eq!(linear_keys(&cfg), vec!["K2".to_string(), "K1".to_string()]);
        assert!(linear_keys(&wire::AppConfig::default()).is_empty());
    }

    /// Linear stores the `contentType` its uploader declared and hands it back on a read, so
    /// the type on the way out is chosen by whoever put the file there. Forwarding it served
    /// `text/html` as `text/html` on an origin with no authentication on any route.
    #[test]
    fn only_an_image_type_comes_off_the_asset_table() {
        for (declared, served) in [
            ("image/png", "image/png"),
            ("image/jpeg", "image/jpeg"),
            ("image/gif", "image/gif"),
            ("image/webp", "image/webp"),
            ("image/avif", "image/avif"),
            ("image/bmp", "image/bmp"),
            // Parameters are not part of the type, and a type is not case-sensitive.
            ("image/png; charset=binary", "image/png"),
            ("IMAGE/PNG", "image/png"),
            ("  image/jpeg  ", "image/jpeg"),
        ] {
            assert_eq!(asset_mime(declared), Some(served), "for {declared}");
        }

        for declared in [
            "text/html",
            "text/html; charset=utf-8",
            "TEXT/HTML",
            "application/xhtml+xml",
            "text/plain",
            "application/pdf",
            "application/octet-stream",
            "video/mp4",
            // An SVG draws AND scripts. `/uploads/:file` serves one because the operator
            // posted it here; nobody chose what sits behind an `assetUrl`.
            "image/svg+xml",
            "image/svg+xml; charset=utf-8",
            "",
            "image",
            "imagepng",
            "image/png/../../text/html",
        ] {
            assert_eq!(asset_mime(declared), None, "should refuse {declared}");
        }
    }

    /// The whole read path against a stub answering exactly what a real workspace asset
    /// answered: a stored HTML file with its three guards.
    ///
    ///     HTTP/2 200
    ///     content-type: text/html
    ///     content-disposition: attachment; filename="linear_download.html"
    ///     content-security-policy: sandbox
    ///     x-content-type-options: nosniff
    ///
    /// Forwarding only `content-type` and `nosniff` put `<script>alert(document.domain)</script>`
    /// on this origin. `nosniff` is no answer at all when the declared type IS `text/html`.
    #[tokio::test]
    async fn a_stored_html_asset_is_refused_and_an_image_is_served() {
        let payload = b"<script>alert(document.domain)</script>".to_vec();
        let png = b"\x89PNG\r\n\x1a\nrmng".to_vec();
        let (html_body, png_body) = (payload.clone(), png.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stub = Router::new()
            .route(
                "/html",
                get(move || {
                    let body = html_body.clone();
                    async move {
                        (
                            [
                                (header::CONTENT_TYPE, "text/html"),
                                (
                                    header::CONTENT_DISPOSITION,
                                    "attachment; filename=\"linear_download.html\"",
                                ),
                                (header::CONTENT_SECURITY_POLICY, "sandbox"),
                                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                            ],
                            body,
                        )
                    }
                }),
            )
            .route(
                "/png",
                get(move || {
                    let body = png_body.clone();
                    async move {
                        (
                            [
                                (header::CONTENT_TYPE, "image/png"),
                                (header::CONTENT_DISPOSITION, "inline; filename=\"shot.png\""),
                                (header::CONTENT_SECURITY_POLICY, "sandbox"),
                            ],
                            body,
                        )
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let keys = vec!["k".to_string()];
        let budget = Duration::from_secs(10);

        let (status, text) =
            asset_response(&app.relay_http, &format!("http://{addr}/html"), &keys, budget)
                .await
                .unwrap_err();
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{text}");
        assert!(text.contains("images only") && text.contains("text/html"), "{text}");
        assert!(!text.contains("<script>"), "the payload must not be echoed: {text}");

        // And nothing that reads as markup survives the type Linear declared, either.
        let mut hostile = HeaderMap::new();
        hostile.insert(header::CONTENT_TYPE, "text/html<script>alert(1)</script>".parse().unwrap());
        let (_, said) = asset_headers(&hostile).unwrap_err();
        assert!(!said.contains('<') && !said.contains('>'), "{said}");

        // And the image the route exists for still comes back, with every guard on it.
        let ok = asset_response(&app.relay_http, &format!("http://{addr}/png"), &keys, budget)
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let head = |name: header::HeaderName| ok.headers()[&name].to_str().unwrap().to_string();
        assert_eq!(head(header::CONTENT_TYPE), "image/png");
        assert_eq!(head(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
        // Linear's own two guards travel with the file rather than being dropped.
        assert_eq!(head(header::CONTENT_DISPOSITION), "inline; filename=\"shot.png\"");
        assert_eq!(head(header::CONTENT_SECURITY_POLICY), "sandbox");
        let bytes = axum::body::to_bytes(ok.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.to_vec(), png);
    }

    /// An upstream that sends neither guard still gets both. `<img>` reads them as nothing,
    /// and a browser pointed straight at the path reads them as "download it, no scripts".
    #[test]
    fn a_stored_file_with_no_guards_of_its_own_is_given_them() {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
        let (mime, disposition) = asset_headers(&h).unwrap();
        assert_eq!((mime, disposition.as_str()), ("image/png", "attachment"));

        // No content type at all is not an image either, and the sentence says what happened.
        let (status, text) = asset_headers(&HeaderMap::new()).unwrap_err();
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(text.contains("nothing"), "{text}");
    }

    /// The deadline bounds the request, not each key attempt.
    ///
    /// Per attempt it is a multiplier: two keys against a target that never answers measured
    /// 40.002s of held connection at a 20-second per-attempt timeout, and five presets would
    /// be 100s, on a route with no authentication. Three keys here, one budget, and the whole
    /// thing has to end inside a budget and a bit rather than three of them.
    #[tokio::test]
    async fn the_asset_deadline_bounds_the_request_and_not_each_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accepts and then never writes a byte, which is the shape a stalled fetch has.
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });

        let app = test_app();
        let keys = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let budget = Duration::from_millis(400);
        let started = std::time::Instant::now();
        let (status, text) =
            asset_response(&app.relay_http, &format!("http://{addr}/x"), &keys, budget)
                .await
                .unwrap_err();
        let took = started.elapsed();

        println!("three keys, one {budget:?} budget, stalled target: {took:?} -> {status} {text}");
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{text}");
        assert!(text.contains("within 400ms"), "{text}");
        // Three keys times the budget would be 1.2s. Generous headroom, and still nowhere
        // near what a per-attempt bound would take.
        assert!(took < budget * 2, "one budget for three keys, took {took:?}");
    }

    /// The bytes are forwarded as they arrive, never collected first.
    ///
    /// Reading the body into a `Vec` measured VmHWM 17604kB to 56604kB for one oversized
    /// fetch, which is memory an unauthenticated caller pins per request. The stub here holds
    /// its second half back until the client has read its first, so a proxy that buffers
    /// cannot answer at all and this test times out instead of passing.
    #[tokio::test]
    async fn an_asset_is_forwarded_as_it_arrives_rather_than_collected_first() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let gate = Arc::new(tokio::sync::Mutex::new(Some(rx)));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stub = Router::new().route(
            "/slow",
            get(move || {
                let gate = gate.clone();
                async move {
                    let stream = futures::stream::once(async move { Ok::<_, std::io::Error>(vec![b'a'; 64 * 1024]) })
                        .chain(futures::stream::once(async move {
                            let rx = gate.lock().await.take().unwrap();
                            let _ = rx.await;
                            Ok::<_, std::io::Error>(vec![b'b'; 16])
                        }));
                    ([(header::CONTENT_TYPE, "image/png")], axum::body::Body::from_stream(stream))
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let (client, upstream) = (app.relay_http.clone(), format!("http://{addr}/slow"));
        let router = Router::new().route(
            "/asset",
            get(move || {
                let (client, upstream) = (client.clone(), upstream.clone());
                async move {
                    asset_response(&client, &upstream, &["k".to_string()], Duration::from_secs(20))
                        .await
                }
            }),
        );
        tokio::spawn(async move { axum::serve(proxy, router).await.unwrap() });

        let mut got = reqwest::Client::new()
            .get(format!("http://{proxy_addr}/asset"))
            .send()
            .await
            .unwrap();
        assert_eq!(got.status(), reqwest::StatusCode::OK);

        // The head of the body arrives while the tail is still held back upstream.
        let first = tokio::time::timeout(Duration::from_secs(5), got.chunk())
            .await
            .expect("a buffering proxy cannot answer until the stub finishes")
            .unwrap()
            .unwrap();
        assert_eq!(first[0], b'a');

        tx.send(()).unwrap();
        let mut rest = first.len();
        while let Some(chunk) = got.chunk().await.unwrap() {
            rest += chunk.len();
        }
        assert_eq!(rest, 64 * 1024 + 16);
    }

    /// The cap holds whether the length was declared or not.
    ///
    /// A declared length past it is refused before a byte is read. Without one the count runs
    /// as the bytes arrive, so a body that declares nothing is cut off at the same place.
    #[tokio::test]
    async fn an_asset_past_the_cap_is_refused_declared_or_not() {
        // Hand-written, because axum sets `content-length` from the body it is given and this
        // stub's whole point is a declared length whose bytes never come.
        let raw = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let raw_addr = raw.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut held = Vec::new();
            while let Ok((mut sock, _)) = raw.accept().await {
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: {}\r\n\r\n",
                    LINEAR_ASSET_MAX_BYTES as u64 + 1
                );
                sock.write_all(head.as_bytes()).await.unwrap();
                sock.flush().await.unwrap();
                held.push(sock);
            }
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stub = Router::new().route(
            "/chunked",
            get(|| async {
                // 33MB in 1MB pieces, no length declared.
                let stream = futures::stream::iter(
                    (0..33).map(|_| Ok::<_, std::io::Error>(vec![0u8; 1024 * 1024])),
                );
                ([(header::CONTENT_TYPE, "image/png")], axum::body::Body::from_stream(stream))
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let keys = vec!["k".to_string()];
        let budget = Duration::from_secs(30);

        let (status, text) =
            asset_response(&app.relay_http, &format!("http://{raw_addr}/declared"), &keys, budget)
                .await
                .unwrap_err();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(text.contains("32MB"), "{text}");

        // No length to go on: the response starts, and the body stops at the cap.
        let ok = asset_response(&app.relay_http, &format!("http://{addr}/chunked"), &keys, budget)
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let cut = axum::body::to_bytes(ok.into_body(), usize::MAX).await;
        assert!(cut.is_err(), "an over-cap body must not read to completion");
    }

    /// Every field of the upload relay is bounded, and each of the three is read once.
    ///
    /// The pre-buffer cap used to guard the `file` arm alone. Measured VmHWM, one process per
    /// case: 60MB in `url` took 19064kB to 146280kB, and 60MB in `headers` with no `url` field
    /// at all took 19244kB to 145720kB. Sizes here are the caps plus a little, because what is
    /// being asserted is which check answers and not how much memory it saves.
    #[tokio::test]
    async fn every_field_of_the_upload_relay_is_bounded_and_read_once() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(test_app())).await.unwrap() });
        let http = reqwest::Client::new();
        let url = format!("http://{addr}/api/linear/upload-relay");
        let send = |ct: String, body: Vec<u8>| {
            let (http, url) = (http.clone(), url.clone());
            async move {
                let resp =
                    http.post(url).header("content-type", ct).body(body).send().await.unwrap();
                (resp.status(), resp.text().await.unwrap())
            }
        };
        let ok_target = "https://storage.googleapis.com/ok";

        // A `url` field past its cap, with no `file` field anywhere in the body.
        let long = format!("https://storage.googleapis.com/{}", "a".repeat(UPLOAD_RELAY_URL_MAX));
        let (ct, body) = multipart(&[("url", long.as_str())], None);
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains("'url' field is larger"), "{text}");

        // A `headers` field past its cap, likewise with no `file` field at all.
        let fat = "h".repeat(UPLOAD_RELAY_HEADERS_MAX + 1);
        let (ct, body) = multipart(&[("headers", fat.as_str())], None);
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains("'headers' field is larger"), "{text}");

        // A second `url` is refused rather than overwriting the first. Without this, a caller
        // spends a whole `file` under a target that was allowed and then names another one.
        let (ct, body) = multipart(&[("url", ok_target), ("url", ok_target)], None);
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains("'url' field was sent twice"), "{text}");

        // So are a second `headers` and a second `file`.
        let (ct, body) = multipart(&[("url", ok_target), ("headers", "[]"), ("headers", "[]")], None);
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains("'headers' field was sent twice"), "{text}");

        // A field this route does not read is drained, not held and not an error: the request
        // still fails on the target it never named, which is the check further down.
        let junk = "j".repeat(4 * 1024 * 1024);
        let (ct, body) = multipart(&[("payload", junk.as_str())], Some(("s.png", b"\x89PNG")));
        let (status, text) = send(ct, body).await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
        assert!(text.contains("upload url is not a URL"), "{text}");
    }

    /// VmHWM for one case, printed rather than asserted.
    ///
    /// This process is the SERVER and `curl` is the client, in a process of its own, because a
    /// 60MB payload costs 60MB to hold on whichever side holds it and only one of those sides
    /// is the thing under test. The body is written to a temp file a megabyte at a time for the
    /// same reason. One case per process, which is what `--exact` gives, and the case comes
    /// from the environment because a peak belongs to a process and not to a test:
    ///
    ///     for c in url headers file-then-url oversized-asset; do \
    ///       RMNG_MEM_CASE=$c cargo test -p control-server \
    ///         web::tests::the_bounded_paths_hold_no_more_than_they_say \
    ///         -- --ignored --exact --nocapture; \
    ///     done
    #[tokio::test]
    #[ignore = "a memory measurement, one case per process, driven by RMNG_MEM_CASE"]
    async fn the_bounded_paths_hold_no_more_than_they_say() {
        use std::io::Write;

        fn vm_hwm() -> String {
            std::fs::read_to_string("/proc/self/status")
                .unwrap_or_default()
                .lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(|l| l.split_whitespace().skip(1).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| "?".into())
        }
        /// One 60MB multipart body on disk, built with a 1MB buffer so writing it is not what
        /// gets measured. `fields` is what goes before and after the big one.
        fn write_body(path: &Path, big_field: &str, before: &[(&str, &str)], after: &[(&str, &str)]) {
            let boundary = "----rmngtestboundary";
            let mut f = std::fs::File::create(path).unwrap();
            let part = |name: &str, value: &str| {
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
            };
            for (name, value) in before {
                f.write_all(part(name, value).as_bytes()).unwrap();
            }
            let head = if big_field == "file" {
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
                     filename=\"big.png\"\r\nContent-Type: image/png\r\n\r\n"
                )
            } else {
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{big_field}\"\r\n\r\n"
                )
            };
            f.write_all(head.as_bytes()).unwrap();
            let mb = vec![b'x'; 1024 * 1024];
            for _ in 0..60 {
                f.write_all(&mb).unwrap();
            }
            f.write_all(b"\r\n").unwrap();
            for (name, value) in after {
                f.write_all(part(name, value).as_bytes()).unwrap();
            }
            f.write_all(format!("--{boundary}--\r\n").as_bytes()).unwrap();
            f.flush().unwrap();
        }

        let case = std::env::var("RMNG_MEM_CASE").unwrap_or_else(|_| "url".into());
        let ok_target = "https://storage.googleapis.com/ok";
        let steal = "https://storage.googleapis.com.example.net/steal";
        let path = std::env::temp_dir().join(format!("rmng-mem-{case}-{}.bin", std::process::id()));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // The asset half needs an upstream. It streams 33MB in 1MB pieces, so the stub's own
        // share of this process stays at one piece.
        let stub_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stub_addr = stub_listener.local_addr().unwrap();
        let stub = Router::new().route(
            "/big",
            get(|| async {
                let stream = futures::stream::iter(
                    (0..33).map(|_| Ok::<_, std::io::Error>(vec![0u8; 1024 * 1024])),
                );
                ([(header::CONTENT_TYPE, "image/png")], axum::body::Body::from_stream(stream))
            }),
        );
        tokio::spawn(async move { axum::serve(stub_listener, stub).await.unwrap() });
        let app = test_app();
        let (client, upstream) = (app.relay_http.clone(), format!("http://{stub_addr}/big"));
        let served = router(app).route(
            "/measure/asset",
            get(move || {
                let (client, upstream) = (client.clone(), upstream.clone());
                async move {
                    asset_response(&client, &upstream, &["k".to_string()], Duration::from_secs(60))
                        .await
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });

        let post = |path: &Path| {
            vec![
                "-X".to_string(),
                "POST".to_string(),
                "-H".to_string(),
                "content-type: multipart/form-data; boundary=----rmngtestboundary".to_string(),
                "--data-binary".to_string(),
                format!("@{}", path.display()),
                format!("http://{addr}/api/linear/upload-relay"),
            ]
        };
        let curl = match case.as_str() {
            // 60MB in the `url` field, and no `file` field anywhere in the body.
            "url" => {
                write_body(&path, "url", &[], &[]);
                post(&path)
            }
            // 60MB in the `headers` field, with no `url` field at all.
            "headers" => {
                write_body(&path, "headers", &[], &[]);
                post(&path)
            }
            // A target that passes, then 60MB of file under it, then a second target.
            "file-then-url" => {
                write_body(&path, "file", &[("url", ok_target)], &[("url", steal)]);
                post(&path)
            }
            // The read half: 33MB back through the streaming path, discarded by the client.
            _ => vec![format!("http://{addr}/measure/asset")],
        };

        println!("case {case}, before: VmHWM {}", vm_hwm());
        let out = tokio::process::Command::new("curl")
            .args(["-sS", "-o", "/dev/null", "-w", "http %{http_code}, %{size_download} bytes down"])
            .args(&curl)
            .output()
            .await
            .unwrap();
        println!(
            "case {case}, answer: {} {}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        println!("case {case}, after:  VmHWM {}", vm_hwm());
        let _ = std::fs::remove_file(&path);
    }

    // What stays here is the control-server half of the boundary: the internal token-delta
    // intake's auth.




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
            if seen.contains("event:stats")
                && seen.contains("event:forwards")
                && seen.contains("event:version")
            {
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
        // The browser reloads when this changes between connections, so it has to be here
        // on every connect and it has to carry a value.
        assert!(
            seen.contains("event:version"),
            "no build identity in: {buf:?}"
        );
        assert!(
            seen.contains("\"buildId\":\"boot-"),
            "build identity should fall back to a boot id with no image label: {buf:?}"
        );
    }

    #[test]
    fn the_build_id_is_stable_until_an_image_revision_replaces_it() {
        let app = App::test_app();
        let boot = app.build_id();

        assert!(boot.starts_with("boot-"), "dev runs get a per-boot id, got {boot}");
        assert_eq!(app.build_id(), boot, "must not change while the process lives");

        // An image built without `GIT_SHA` labels an empty revision; keep the boot id rather
        // than publish an empty identity every client would compare equal.
        app.set_build_id("");
        assert_eq!(app.build_id(), boot);

        app.set_build_id("2ae7f50");
        assert_eq!(app.build_id(), "2ae7f50");
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

}
