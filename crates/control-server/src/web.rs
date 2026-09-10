//! Port 2 — the web API + SSE + static frontend. Phase 1 + the Phase-2 clone/
//! delete surface; the rest (Linear/Claude/chat/config/…) lands as those modules
//! are ported.

use std::convert::Infallible;
use std::path::Path;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path as AxPath, State},
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
use crate::naming;

pub fn router(app: App) -> Router {
    let routes = Router::new()
        .route("/events", get(events))
        .route("/api/state", get(state_get))
        .route("/api/stats", get(stats_get))
        .route("/api/activate", post(activate))
        .route("/api/board", put(board_put))
        .route("/api/tickets/order", put(ticket_order_put))
        // Which clones the operator has silenced. A browser-notification filter, stored here so
        // one mute covers every tab and the phone.
        .route("/api/clones/muted", put(muted_put))
        .route("/api/clone", post(clone))
        .route("/api/fork", post(fork))
        .route("/api/hosts/:id/rebase", post(rebase))
        .route("/api/layout/activate", post(layout_activate))
        .route("/api/delete", post(delete))
        .route("/api/notes/:id", get(notes_get).put(notes_save))
        .route("/api/upload", post(upload))
        .route("/api/linear/upload-relay", post(linear_upload_relay))
        .route("/api/linear/asset", get(linear_asset))
        .route("/uploads/:file", get(uploads_serve))
        // Distilled clone transcripts, kept after the clone is gone (see `crate::ledger`). Both
        // run the read server-side, so a caller gets matches back instead of a corpus.
        .route("/api/ledger/search", get(ledger_search))
        .route("/api/ledger/read", get(ledger_read))
        .route("/api/config", get(config_get).put(config_put))
        .route("/api/config/test", post(config_test))
        .route("/api/setup/env", get(setup_env))
        .route("/api/server/version", get(server_version))
        .route("/api/server/update", post(server_update))
        .route("/api/server/restart", post(server_restart))
        .route("/api/images/prebuild", post(images_prebuild))
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
        .route("/api/self", get(clone_self))
        // Claude + Codex accounts. The server owns each account's OAuth refresh lifecycle and
        // pushes only short-lived access tokens into clones; these twelve are symmetric across
        // the two providers. Account POOLS are not edited here — they live in `config.json`
        // (`cloneGroups`/`codexGroups`) and are saved wholesale through `PUT /api/config`.
        // Sign in here instead of in a clone: begin hands back a URL, complete takes the
        // callback URL the browser landed on. See `crate::oauth`.
        .route("/api/login/begin", post(login_begin))
        .route("/api/login/complete", post(login_complete))
        .route("/api/claude/refresh", post(claude_refresh))
        .route("/api/claude/swap", post(claude_swap))
        .route("/api/claude/delete", post(claude_delete))
        .route("/api/claude/rotate", post(claude_rotate))
        .route("/api/codex/refresh", post(codex_refresh))
        .route("/api/codex/swap", post(codex_swap))
        .route("/api/codex/delete", post(codex_delete))
        .route("/api/codex/rotate", post(codex_rotate));

    // Frontend from the filesystem: the image's /usr/local/share/rmng/static, else the
    // repo dev build (see `assets::static_dir`). No override: the old `static_dir` config
    // (dev hot-reload without a rebuild) is gone with the Advanced pane.
    let dir = crate::assets::static_dir();
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
    let port = wire::PORT_WEB;
    let router = router(app);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("port 2 (web API + SSE + static) on http://{addr}");
    // `with_connect_info` is what puts the peer address in reach of a handler, and
    // [`caller_clone`] needs it: a clone's address on the rmng bridge is the one thing about
    // the caller that Docker assigns and nothing inside the clone can restate.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
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
        futures::stream::once(async move { Ok(Event::default().event("version").data(build_id)) });

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
    // Overlay the live daemon sessions: `store` persists rows, but connectedness
    // changes outside mutations (a Hello arrives anytime), so it is resolved here.
    let mut state = app.store.get();
    for h in &mut state.hosts {
        h.daemon_connected = app.media.is_connected(&h.id);
    }
    Json(state)
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
    // The viewer never follows an archived clone. Its container is stopped, so there is no
    // stream to show and nothing to deliver its input to, and the selection would leave the
    // viewer holding a still frame that looks live.
    //
    // The dashboard can still open an archived clone's chat and notes: that focus is the
    // browser's own, alongside an open ticket, and it never reaches this endpoint. So the
    // request keeps the selection where it was rather than failing, because the click that
    // produced it is a navigation, not a command.
    let req = match req.id.as_deref() {
        Some(id)
            if app
                .store
                .get()
                .hosts
                .iter()
                .any(|h| h.id == id && h.archived) =>
        {
            ActivateReq {
                id: app.store.get().selected,
            }
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
    let state = app.store.mutate(|s| {
        // Selecting a clone acknowledges its prior working→not-working transition.
        if let Some(id) = req.id.as_deref() {
            if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                h.unread = false;
            }
        }
        s.selected = req.id;
    });
    // The clone coming on screen takes the active layout now. It kept the monitors it was
    // last viewed with while the operator was elsewhere, so this is where a layout preset
    // activated in the meantime reaches it. Same-layout clones are unaffected: their session
    // holder drops a layout equal to the one it holds, and no window moves.
    if state.selected != previously_selected {
        if let Some(id) = state.selected.as_deref() {
            crate::mediaplane::apply_active_layout(&app, id);
        }
    }
    Json(state)
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
struct MutedPutReq {
    clone_ids: Vec<String>,
}

/// Replace the muted-clone set wholesale (`PUT /api/clones/muted`).
///
/// Muting is a browser-notification filter and nothing else: this server raises no
/// notifications, so it stores the set and reports it back over SSE. Every open tab and phone
/// then agrees on which clones are silent, which a per-browser store could not do.
///
/// Sorted and deduplicated, because the set has no order of its own and a client that sends one
/// twice should not make `state.json` churn. Ids for clones that no longer exist are kept, the
/// same bargain as [`ticket_order_put`]: an entry that matches nothing costs one string, and a
/// clone recreated under the same name is the one the operator muted anyway.
async fn muted_put(State(app): State<App>, Json(req): Json<MutedPutReq>) -> Json<ControlState> {
    let mut ids: Vec<String> = req
        .clone_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Json(app.store.mutate(|s| s.muted_clones = ids))
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
    let port = wire::PORT_DAEMON_MCP;
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
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            let named = !k.is_empty()
                && k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            named.then(|| format!("{k}={}", unquote_shell_value(v)))
        })
        .collect()
}

/// One value as `systemctl show-environment` prints it, with systemd's shell quoting undone.
///
/// It prints through systemd's `shell_maybe_quote`, so a value carrying a bracket, a space, a
/// quote, a dollar or a control byte comes back as `$'...'` with C escapes inside. Taking that
/// verbatim put the quoting INTO the value: a clone whose session held
/// `ANTHROPIC_MODEL=opus[1m]` handed every `rmng clone exec` `$'opus[1m]'`, and Claude Code
/// refused to start against a model by that name.
///
/// The escape set is what systemd's own output was measured to produce, not what the shell
/// grammar allows: `it's` comes back `$'it\'s'`, `back\slash` as `$'back\\slash'`, a tab as
/// `\t`, BEL as `\a`, byte 1 as `\001`, and `café` is not quoted at all. Octal and hex escapes
/// yield a BYTE, so this decodes into bytes and reads UTF-8 back out of them: `\303\251` is one
/// `é`, never two characters of mojibake.
fn unquote_shell_value(v: &str) -> String {
    let Some(inner) = v
        .strip_prefix("$'")
        .and_then(|rest| rest.strip_suffix('\''))
    else {
        return v.to_string();
    };
    let src = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] != b'\\' {
            out.push(src[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = src.get(i) else {
            // A trailing backslash is not an escape of anything. Keep it.
            out.push(b'\\');
            break;
        };
        i += 1;
        match c {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'x' => {
                let end = (i + 2).min(src.len());
                let digits = &src[i..end];
                let take = digits.iter().take_while(|b| b.is_ascii_hexdigit()).count();
                match take {
                    0 => out.push(b'x'),
                    n => {
                        let hex = std::str::from_utf8(&src[i..i + n]).unwrap_or_default();
                        out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                        i += n;
                    }
                }
            }
            b'0'..=b'7' => {
                // Up to three octal digits, counting the one already taken.
                let end = (i + 2).min(src.len());
                let more = src[i..end]
                    .iter()
                    .take_while(|b| (b'0'..=b'7').contains(b))
                    .count();
                let oct = std::str::from_utf8(&src[i - 1..i + more]).unwrap_or_default();
                out.push(u8::from_str_radix(oct, 8).unwrap_or(b'?'));
                i += more;
            }
            // `\\`, `\'`, `\"`, and anything else: the character itself.
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Merge caller `overrides` (`KEY=VAL`) over a `base` env, caller-wins: any base entry whose key a
/// caller entry also sets is dropped, then the overrides are appended.
fn merge_env(base: &mut Vec<String>, overrides: &[String]) {
    let keys: std::collections::HashSet<&str> = overrides
        .iter()
        .filter_map(|e| e.split_once('=').map(|(k, _)| k))
        .collect();
    base.retain(|e| {
        e.split_once('=')
            .map(|(k, _)| !keys.contains(k))
            .unwrap_or(true)
    });
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
///
/// Whatever the manager reports is the result: gen-2 images carry no stale container
/// `Config.Env`, so nothing needs cancelling on top of the session env.
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

/// The managed clone reachable at `ip` on the rmng bridge, when exactly one is.
///
/// `local_ip` is refreshed from a Docker inspect on every monitor tick (4 s), so a recreated
/// container's new address is current within one tick. Two rows claiming one address means the
/// map is mid-refresh and no answer is trustworthy, so that yields `None` rather than a guess.
fn clone_at_ip(app: &App, ip: std::net::IpAddr) -> Option<String> {
    let ip = ip.to_canonical(); // an IPv4 peer on a dual-stack listener arrives as ::ffff:a.b.c.d
    let mut hit = None;
    for h in app.store.get().hosts.iter().filter(|h| h.managed) {
        if h.local_ip.as_deref().and_then(|s| s.parse().ok()) != Some(ip) {
            continue;
        }
        if hit.is_some() {
            return None;
        }
        hit = Some(h.id.clone());
    }
    hit
}

/// Which clone is calling: its address first, then the two identity headers `control-client`
/// sends.
///
/// **The peer address decides.** A clone reaches the server over the rmng bridge, container to
/// container, so the address on the connection is the one Docker gave that container. Nothing
/// running inside the clone chooses it, nothing can inherit it from an image, and it cannot go
/// stale, which is what separates it from everything else the caller could say about itself.
///
/// The headers are the fallback for a caller whose address does not name a clone: the fleet CLI
/// run on the operator's box against a remote server, a request through a proxy, dev mode.
/// `X-RMNG-Proxy-Key` says whether the caller is inside the fleet at all, being present in a
/// clone's environment and absent on a laptop, and `X-RMNG-Clone` carries the caller's container
/// hostname, which is the clone id. Between those two the hostname wins.
///
/// The key alone was wrong for a fleet's worth of clones: it travels through `/etc/environment`
/// into the lingering user manager and then into every session child, a committed image bakes
/// the source clone's copy, and a process keeps its launch environment for life. Seven clones on
/// the production fleet ran their whole desktop session (terminals, editors, agents) under the
/// identity of the clone their image was committed from.
fn caller_clone(app: &App, headers: &HeaderMap, peer: Option<std::net::IpAddr>) -> Option<String> {
    if let Some(id) = peer.and_then(|ip| clone_at_ip(app, ip)) {
        return Some(id);
    }
    let header = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    let by_key = app
        .clone_keys
        .clone_for_token(header("x-rmng-proxy-key").filter(|k| !k.is_empty())?);
    let by_host = header("x-rmng-clone")
        .filter(|h| !h.is_empty())
        .filter(|h| {
            app.store
                .get()
                .hosts
                .iter()
                .any(|c| c.id == *h && c.managed)
        })
        .map(str::to_string);
    match (by_host, by_key) {
        (Some(host), Some(key)) if host != key => {
            tracing::warn!(
                "clone {host} presented {key}'s identity key, so the hostname decides. Its \
                 /etc/environment was inherited from a committed image; the clone heals on its \
                 next container restart"
            );
            Some(host)
        }
        (Some(host), _) => Some(host),
        (None, by_key) => by_key,
    }
}

/// `GET /api/self` — the clone record of whoever is calling.
///
/// Identity is [`caller_clone`]: the address the request arrived on, and the headers only when
/// that address names no clone. A caller the server cannot place is not inside a clone, which is
/// a 404 rather than an error: it is a legitimate answer for the operator laptop.
///
/// `ConnectInfo` is optional so the handler still answers under a test server built without it.
async fn clone_self(
    State(app): State<App>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
) -> Result<Json<wire::RmngClone>, (StatusCode, String)> {
    let id = caller_clone(&app, &headers, peer.map(|p| p.0.ip())).ok_or((
        StatusCode::NOT_FOUND,
        "not running inside a managed clone".to_string(),
    ))?;
    clone_by_id(&app, &id)
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, format!("no clone '{id}'")))
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

/// The hostname for a new clone, plus the display name that goes with it: a duplicate ticket
/// gets the next free hostname and its suffix in the name ("title (a)").
fn derive_hostname(app: &App, base: &str, title: &str) -> (String, String) {
    let hostname = jobs::next_free_hostname(app, base);
    let suffix = hostname.strip_prefix(base).unwrap_or("").to_string();
    let display = if suffix.is_empty() {
        title.to_string()
    } else {
        format!("{title} ({suffix})")
    };
    (hostname, display)
}

/// `POST /api/clone` — start a template clone: `{ plain: { title, message } }` plus an
/// optional `preset` name. The hostname derives server-side from the title, and the image
/// builds on demand from the preset's Dockerfile. Async — returns `{ ok: true, op }`;
/// progress streams over `/events`. Unknown fields are ignored.
async fn clone(
    State(app): State<App>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let str_field = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let cfg = app.config();
    let prefix = cfg.docker.hostname_prefix.clone();

    // An explicitly chosen preset (by name); "auto"/"none"/empty means none. Unknown → 400.
    let explicit = match str_field("preset")
        .map(|s| s.trim().to_string())
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
    let plain = body
        .get("plain")
        .filter(|v| v.is_object())
        .ok_or_else(|| bad("body must include { plain: { title } }".into()))?;
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
    // A preset must be picked whenever any are configured.
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
    // The preset's own account defaults, if it names any; `None` reads as `auto` downstream.
    let preset_default = |pick: fn(&wire::Preset) -> &str| -> Option<String> {
        explicit
            .map(|p| pick(p).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    // Startup script: default on everywhere, opt out per request.
    let run_startup_script = body
        .get("runStartupScript")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let (hostname, display) =
        derive_hostname(&app, &naming::plain_hostname_base(&prefix, &title), &title);
    let spec = CloneSpec {
        source_image: String::new(),
        new_hostname: hostname,
        linear: Some(LinearMeta {
            display_name: Some(display),
            ..Default::default()
        }),
        claude_account: preset_default(|p| &p.claude_account),
        codex_account: preset_default(|p| &p.codex_account),
        first_message: Some(message).filter(|m| !m.is_empty()),
        agent_instructions: None,
        claude_instructions: None,
        preset_name: explicit.map(|p| p.name.clone()),
        env,
        agent_playbook: compose_playbook(&cfg, explicit),
        global_prompt: compose_global_prompt(&cfg, explicit),
        headless: false,
        parent: None,
        run_startup_script,
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
pub(crate) fn compose_global_prompt(
    cfg: &wire::AppConfig,
    preset: Option<&wire::Preset>,
) -> String {
    let base = cfg.global_prompt.trim();
    match preset
        .map(|p| p.global_prompt.trim())
        .filter(|s| !s.is_empty())
    {
        Some(extra) => format!("{base}\n\n{extra}"),
        None => base.to_string(),
    }
}

// --- derived images (gen-2 preset builds) -----------------------------------

/// `POST /api/images/prebuild` — warm a preset image without creating: build the posted
/// Dockerfile text on miss. The preset card's rebuild button posts the editor's current
/// text (which may be unsaved). Returns the driving Operation (kind `prebuild`).
async fn images_prebuild(
    State(app): State<App>,
    Json(body): Json<PrebuildReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    jobs::start_prebuild(&app, body.dockerfile)
        .map(|op| Json(json!({ "ok": true, "op": op })))
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
struct PrebuildReq {
    /// Full Dockerfile text to build (the preset editor's current text).
    #[serde(default)]
    dockerfile: String,
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

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForkReq {
    /// Source gen-2 clone id.
    source: String,
    /// Headless (no desktop) fork.
    #[serde(default)]
    headless: bool,
    /// Preset name override (`None` = inherit the source preset).
    #[serde(default)]
    preset: Option<String>,
    /// Ticket metadata override (`None` = inherit the source ticket context).
    #[serde(default)]
    linear: Option<jobs::LinearMeta>,
    /// Claude account selection override (`None` = inherit).
    #[serde(default)]
    claude_account: Option<String>,
    /// Codex account selection override (`None` = inherit).
    #[serde(default)]
    codex_account: Option<String>,
    /// First message override for the agent kickoff.
    #[serde(default)]
    first_message: Option<String>,
    /// Instruction overrides for the agent kickoff.
    #[serde(default)]
    agent_instructions: Option<String>,
    #[serde(default)]
    claude_instructions: Option<String>,
    /// Run the preset's startup script as the clone user. Default on; opt out per request.
    #[serde(default = "default_true")]
    run_startup_script: bool,
}

/// `POST /api/fork` — fork a gen-2 clone (`{ source }` plus the optional
/// ticket/preset/account overrides above): snapshot + clone the source home, create
/// from its recorded base tag. Returns the driving Operation.
async fn fork(
    State(app): State<App>,
    Json(req): Json<ForkReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = app.config();
    let prefix = cfg.docker.hostname_prefix.as_str();
    // The hostname always derives server-side from the ticket identifier or title,
    // like create does (uniqueness needs the live clone list, which no client
    // can see). No caller names the fork itself.
    let base = match req
        .linear
        .as_ref()
        .and_then(|l| l.ticket.clone())
        .filter(|t| !t.is_empty())
    {
        Some(ticket) => naming::ticket_hostname_base(prefix, &ticket),
        None => {
            let title = req
                .linear
                .as_ref()
                .and_then(|l| l.display_name.clone())
                .unwrap_or_default();
            naming::plain_hostname_base(prefix, &title)
        }
    };
    let hostname = derive_hostname(&app, &base, "").0;
    jobs::start_fork(
        &app,
        jobs::ForkSpec {
            source_id: req.source.trim().to_string(),
            new_hostname: hostname,
            headless: req.headless,
            preset_name: req.preset,
            linear: req.linear,
            claude_account: req.claude_account,
            codex_account: req.codex_account,
            first_message: req.first_message,
            agent_instructions: req.agent_instructions,
            claude_instructions: req.claude_instructions,
            run_startup_script: req.run_startup_script,
        },
    )
    .map(|op| Json(json!({ "ok": true, "op": op })))
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
struct RebaseReq {
    /// Target preset: the clone's system image becomes this preset's image (built on
    /// miss). The dataset, id, and the clone's own preset bindings are kept.
    #[serde(default)]
    preset: String,
    /// Rebuild the preset image even when its tag exists (a base release under the
    /// same tag does not invalidate it otherwise).
    #[serde(default)]
    rebuild: bool,
}

/// `POST /api/hosts/:id/rebase` — rebase a gen-2 clone onto a preset's image.
/// The old container is replaced (name == id); on failure the old tag auto-recreates.
/// Returns the driving Operation.
async fn rebase(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(req): Json<RebaseReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    jobs::start_rebase(&app, &id, &req.preset, req.rebuild)
        .map(|op| Json(json!({ "ok": true, "op": op })))
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

/// `POST /api/layout/activate`: make `name` the active layout preset and live-apply it to
/// the clone on screen (no session restart). Persists config, mirrors the active name into
/// ControlState (so all sidebars update over SSE), then pushes `SetMonitors` to the selected
/// clone's daemon. Best-effort; a failure is reported.
///
/// The rest of the fleet keeps the monitors it was last viewed with and takes the new layout
/// on the switch to it, in `activate`. Pushing to everything at once rebuilds every clone's
/// Mutter session in the same second, which stalls the whole board.
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

    // 3. Live-apply to the clone the operator is watching, and to that one only.
    let monitors = cfg.effective_monitors();
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    if let Some(id) = app.store.selected() {
        // A selection with no daemon behind it (headless, archived, still booting) is not a
        // failure of this request: it has no monitors to change.
        if app.media.is_connected(&id) {
            match app.media.set_monitors(&id, &monitors) {
                Ok(()) => applied.push(id),
                Err(e) => errors.push(format!("{id}: {e}")),
            }
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
    let blocks = files::load_notes(&app.data_dir(), &id).unwrap_or_default();
    Json(json!({ "blocks": blocks }))
}

async fn notes_save(
    State(app): State<App>,
    AxPath(id): AxPath<String>,
    Json(body): Json<NotesBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    files::save_notes(&app.data_dir(), &id, &body.blocks)
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
            let url = files::save_upload(&app.data_dir(), &ct, &bytes)
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

/// How long the PUT may take before this gives up on it.
///
/// Linear signs the URL with `X-Goog-Expires=60`, and a PUT 68 seconds after issue comes back
/// 400 `ExpiredToken`. The browser has already spent part of that minute on the mutation and
/// on posting the bytes here, so a hop still running at 45 seconds has missed the window. The
/// caller is better served by this sentence than by waiting for the bucket to say so.
const UPLOAD_RELAY_TIMEOUT_SECS: u64 = 45;

/// Headers that describe this hop rather than the upload, and would break the replayed PUT.
///
/// `content-length` is reqwest's to set from the body it is given, and `host` is reqwest's to
/// set from the URL it is dialing. The other two are hop-by-hop by definition.
const UPLOAD_RELAY_DROPPED: [&str; 4] =
    ["host", "content-length", "transfer-encoding", "connection"];

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
/// The whole body is bounded by the router's 64MB `DefaultBodyLimit`.
async fn linear_upload_relay(
    State(app): State<App>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let mut url = String::new();
    let mut headers_raw = String::new();
    let mut body: Option<Vec<u8>> = None;

    while let Some(field) = mp.next_field().await.map_err(|e| bad(e.to_string()))? {
        match field.name().unwrap_or("").to_string().as_str() {
            "url" => url = field.text().await.map_err(|e| bad(e.to_string()))?,
            "headers" => headers_raw = field.text().await.map_err(|e| bad(e.to_string()))?,
            "file" => {
                body = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| bad(e.to_string()))?
                        .to_vec(),
                );
            }
            _ => {}
        }
    }

    let body = body.ok_or_else(|| bad("no 'file' field".into()))?;
    // Absent means no headers at all, which the bucket answers with a 403. That is the caller's
    // mistake to make, and reading it as "send nothing" beats guessing a content type.
    let headers = if headers_raw.trim().is_empty() {
        Vec::new()
    } else {
        relay_headers(&headers_raw).map_err(bad)?
    };

    let resp = relay_request(&app.http, url.trim(), &headers)
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
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("linear upload relay: bucket answered {status}: {text}");
        return Err((StatusCode::BAD_GATEWAY, relay_failure(status, &text)));
    }
    Ok(Json(json!({ "ok": true, "status": status })))
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

/// The most one asset may be served as. Counted as the bytes arrive, so a body that declares
/// no length or lies about one is cut off at the same place a declared one is refused.
const LINEAR_ASSET_MAX_BYTES: usize = 32 * 1024 * 1024;

/// How long the WHOLE request may take: every key attempt and the body, on one clock.
///
/// Per attempt this would be a multiplier rather than a bound. With two keys against a target
/// that never answers, a per-attempt 20 seconds measured 40.002s of held connection, and five
/// presets would be 100s. One wedged Linear connection holds one socket for this long and no
/// longer, however many presets are configured.
const LINEAR_ASSET_TIMEOUT_SECS: u64 = 20;

/// The asset's bytes, forwarded as they arrive, still under the cap and still on the clock.
///
/// Nothing between Linear and the browser holds the whole file. Reading it into a `Vec` first
/// measured VmHWM 17604kB to 56604kB for one oversized fetch, which is 32MB of resident memory
/// for one image. `deadline` is the same instant the key loop ran under, so a body that
/// trickles cannot outlast the request that asked for it.
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
/// Separate from the handler so a test can point it at a stub, the way [`relay_request`] is.
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
        // together, so that adding a preset cannot add 20 seconds to how long one hop runs.
        let resp = match http
            .get(url)
            .header(header::AUTHORIZATION, key)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last = format!("the asset host was unreachable: {e}");
                continue;
            }
        };

        if !resp.status().is_success() {
            // 401 and 403 are "not this key", which is the whole reason for the loop. Every
            // other status is remembered the same way, so what gets reported is the last
            // answer seen rather than a guess about which key should have worked.
            last = format!(
                "Linear answered HTTP {} for that asset",
                resp.status().as_u16()
            );
            continue;
        }
        return Ok(resp);
    }

    // What Linear said about the last key tried, which is the answer that describes the file.
    Err((StatusCode::BAD_GATEWAY, last))
}

/// One asset read and made into a response, under one deadline for the whole of it.
///
/// Split from the handler for the reason [`fetch_asset`] is: a stub is how a test drives the
/// key loop, the deadline, and the size cap. `budget` is that deadline, and it is what the
/// handler spends [`LINEAR_ASSET_TIMEOUT_SECS`] on.
async fn asset_response(
    http: &reqwest::Client,
    url: &str,
    keys: &[String],
    budget: Duration,
) -> Result<Response, (StatusCode, String)> {
    // One deadline, taken once and carried into the body stream. Every key attempt and every
    // byte runs under it, so one hop takes this long and never a multiple of it.
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

    // Whatever Linear declared. It stores the `contentType` its uploader sent and hands the
    // same string back on a read, so this is the only description of the bytes that exists.
    let mime = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    // A length past the cap is refused before a byte is read, so the common oversized case
    // costs one round trip and the caller gets a status rather than a cut-off image.
    if resp
        .content_length()
        .is_some_and(|n| n > LINEAR_ASSET_MAX_BYTES as u64)
    {
        return Err((StatusCode::BAD_GATEWAY, asset_too_large()));
    }
    Ok((
        [
            (header::CONTENT_TYPE, mime),
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
    let keys = linear_keys(&app.config());
    asset_response(
        &app.http,
        q.url.trim(),
        &keys,
        Duration::from_secs(LINEAR_ASSET_TIMEOUT_SECS),
    )
    .await
}

// --- transcript ledger (see `crate::ledger`) --------------------------------

#[derive(Deserialize)]
struct LedgerSearchQuery {
    /// The pattern, as a case-insensitive substring of the whole ledger line.
    q: String,
    /// One clone id, or absent for every clone the ledger knows.
    #[serde(default)]
    clone: Option<String>,
    /// Epoch milliseconds, both inclusive.
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    until: Option<i64>,
    /// `true` keeps only subagent turns, `false` only the conversation, absent keeps both.
    #[serde(default)]
    sidechain: Option<bool>,
    /// One subagent's id, as a record's `agentId`.
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /api/ledger/search?q=&clone=&since=&until=&limit=`: grep every clone's distilled
/// transcripts, including clones that no longer exist.
///
/// The search runs here rather than in the caller, so an assistant asking how a piece of work was
/// done gets the matching lines back instead of the corpus. Each hit carries the byte offset of
/// its line, which is what [`ledger_read`] takes to read the conversation around it.
async fn ledger_search(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<LedgerSearchQuery>,
) -> Result<Json<crate::ledger::SearchResult>, (StatusCode, String)> {
    let pattern = q.q.trim().to_string();
    if pattern.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "q is required".into()));
    }
    let data_dir = app.data_dir();
    let query = crate::ledger::SearchQuery {
        pattern,
        clone: q
            .clone
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty()),
        since_ms: q.since,
        until_ms: q.until,
        sidechain: q.sidechain,
        agent: q
            .agent
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty()),
        limit: q.limit.unwrap_or_else(crate::ledger::default_limit),
    };
    // Blocking: one search can read every ledger file on disk.
    tokio::task::spawn_blocking(move || crate::ledger::search(&data_dir, &query))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Deserialize)]
struct LedgerReadQuery {
    clone: String,
    session: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    len: Option<u64>,
}

/// `GET /api/ledger/read?clone=&session=&offset=&len=`: a byte range of one session's ledger.
///
/// The range is snapped outward to line boundaries, so the response is always whole NDJSON lines.
/// Pass an offset below a hit's own to read what led up to it.
async fn ledger_read(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<LedgerReadQuery>,
) -> Result<Json<crate::ledger::Range>, (StatusCode, String)> {
    let data_dir = app.data_dir();
    let offset = q.offset.unwrap_or(0);
    let len = q.len.unwrap_or(64 * 1024);
    tokio::task::spawn_blocking(move || {
        crate::ledger::read_range(&data_dir, q.clone.trim(), q.session.trim(), offset, len)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// `GET /uploads/:file` — serve a stored upload by its generated name.
async fn uploads_serve(State(app): State<App>, AxPath(file): AxPath<String>) -> Response {
    match files::read_upload(&app.data_dir(), &file) {
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
    // Fan out content convergence: prompts, presets, and keys changed above reach running
    // clones now — env, parity, and MCP merges re-resolve from the saved config. Detached:
    // the PUT must not wait on Docker calls to a wedged clone (same reasoning as the SSH
    // push above); each step is stamped and idempotent, so a slow fleet just converges late.
    {
        let app = app.clone();
        tokio::spawn(async move {
            crate::clone_reconcile::sync_all_running(&app, "settings-save").await;
        });
    }
    // Editing the active preset's geometry is the same kind of change as activating another
    // preset, so it lands the same way: on the clone the operator is watching, now, and on
    // every other clone when they switch to it. Compared rather than assumed, because most
    // config writes touch nothing to do with monitors.
    if old.effective_monitors() != merged.effective_monitors() {
        if let Some(id) = app.store.selected() {
            crate::mediaplane::apply_active_layout(&app, &id);
        }
    }
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
    /// A value to test INSTEAD of the stored one, for a credential the operator has typed
    /// but not saved. Without it, "paste key, press Test" would test the previous key and
    /// report on something the operator is not looking at.
    #[serde(default)]
    value: String,
    /// Same idea for a second unsaved field, used by `"judge"`: `value` carries the account
    /// and this carries the model.
    #[serde(default)]
    model: String,
}

/// `POST /api/config/test` — validate a setting from the UI. `"docker"` re-runs the Docker
/// self-setup probe and collapses the [`crate::docker::EnvReport`] into a single
/// `(ok, message)` verdict (the row-by-row breakdown is `GET /api/setup/env`).
/// `"judge"` puts one real stuck-detection question to GPT on an imported Codex account.
async fn config_test(State(app): State<App>, Json(req): Json<TestReq>) -> Json<serde_json::Value> {
    let (ok, message) = match req.what.as_str() {
        "docker" => {
            let setup_complete = app.config().setup_complete;
            let report = app.docker.self_setup(setup_complete).await;
            collapse_env_report(&report)
        }
        "judge" => {
            let stored = app.config().judge;
            let model = if req.model.is_empty() {
                stored.codex_model
            } else {
                req.model
            };
            crate::stuck::probe_codex(&app, &req.value, &model).await
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

/// `POST /api/claude/import/check` — confirm a clone is signed in to Claude Code via
/// claude.ai and report the account identity (so the UI can show it before the
/// operator mints + pastes a long-lived token).
#[derive(Deserialize)]
struct LoginBeginReq {
    /// `"claude"` or `"codex"`.
    provider: String,
}

/// `POST /api/login/begin` — start an account sign-in and return the URL to open.
///
/// Nothing is stored against an account yet. What is held is the PKCE verifier, keyed by
/// the `state` that will come back in the callback, until the paste or the timeout.
async fn login_begin(State(app): State<App>, Json(req): Json<LoginBeginReq>) -> JsonResult {
    let provider = crate::oauth::Provider::parse(&req.provider).ok_or_else(|| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("unknown provider '{}'", req.provider),
        )
    })?;
    let url = crate::oauth::begin(&app, provider)
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(serde_json::json!({ "url": url })))
}

#[derive(Deserialize)]
struct LoginCompleteReq {
    provider: String,
    /// Whatever the operator copied: the callback URL, its query, or `code=…&state=…`.
    pasted: String,
    /// The pool to join, or empty for none. Must already exist. Ignored when `replaces` is
    /// set: a replacement inherits the pools of the account it stands in for.
    #[serde(default)]
    group: String,
    /// An imported account this sign-in stands in for, or empty for a plain import. The
    /// replaced account's pools and clones move to the new one, and it is then deleted.
    #[serde(default)]
    replaces: String,
}

/// `POST /api/login/complete` — redeem the pasted callback and store the account.
///
/// With `replaces` set, the stored account then takes over from that one (pools, pinned
/// clones, current assignments) and the old one is deleted. That is the whole of the
/// "sign in again" badge: recovering a dead account used to mean deleting it and importing
/// its replacement by hand, rebuilding its pools and pins from memory.
///
/// A 400 covers everything the operator can fix by pasting again; the provider refusing the
/// code is one of those, so it is not a 502.
async fn login_complete(State(app): State<App>, Json(req): Json<LoginCompleteReq>) -> JsonResult {
    let provider = crate::oauth::Provider::parse(&req.provider).ok_or_else(|| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("unknown provider '{}'", req.provider),
        )
    })?;
    let replaces = req.replaces.trim().to_string();
    // A replacement joins its predecessor's pools, so the modal's pool pick is not asked for
    // and must not be applied on top of them.
    let group = if replaces.is_empty() {
        req.group.as_str()
    } else {
        ""
    };
    let email = crate::oauth::complete(&app, provider, &req.pasted, group)
        .await
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    let mut moved: Vec<String> = Vec::new();
    if !replaces.is_empty() {
        // The account is stored either way. A failure here leaves it imported alongside the
        // one it was meant to replace, which is recoverable by hand, so it reports rather
        // than pretending the sign-in did not happen.
        moved = match provider {
            crate::oauth::Provider::Claude => {
                crate::claude::replace_account(&app, &replaces, &email).await
            }
            crate::oauth::Provider::Codex => {
                crate::codex::replace_account(&app, &replaces, &email).await
            }
        }
        .map_err(|e| {
            err_json(
                StatusCode::BAD_REQUEST,
                format!("{email} was signed in, but taking over from {replaces} failed: {e:#}"),
            )
        })?;
    }

    // Put its usage on screen without making the browser wait, exactly as the clone import
    // does. The account is already stored by the line above.
    let app2 = app.clone();
    tokio::spawn(async move {
        match provider {
            crate::oauth::Provider::Claude => crate::claude::poll_once(&app2).await,
            crate::oauth::Provider::Codex => crate::codex::poll_once(&app2).await,
        }
    });
    Ok(Json(serde_json::json!({
        "ok": true,
        "email": email,
        "replaced": if replaces.is_empty() { None } else { Some(replaces) },
        "moved": moved,
    })))
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
    let assignment = crate::claude::resolve_assignment(
        &app,
        Some(&req.account),
        host.claude_account_email.as_deref(),
    )
    .ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "no Claude account can take this clone: none is imported, or every one \
                     that could has a token that expired and cannot be refreshed"
                .into(),
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
    let assignment = crate::codex::resolve_assignment(
        &app,
        Some(&req.account),
        host.codex_account_email.as_deref(),
    )
    .ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "no Codex account can take this clone: none is imported, or every one that \
                 could has a token that expired and cannot be refreshed"
                .into(),
        )
    })?;
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
        let cfg = wire::AppConfig::default();
        App::new(store, cfg, &dir.to_string_lossy())
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

        board_put(
            State(app.clone()),
            Json(BoardPutReq {
                columns: vec![column("todo", &["a"])],
            }),
        )
        .await;
        // A second write is a replacement, not a merge: the operator deleting a column has to
        // be able to make the board smaller.
        let after = board_put(
            State(app.clone()),
            Json(BoardPutReq {
                columns: vec![column("doing", &[])],
            }),
        )
        .await;

        assert_eq!(after.0.board_columns, vec![column("doing", &[])]);
        assert_eq!(app.store.get().board_columns, vec![column("doing", &[])]);
    }

    #[tokio::test]
    async fn an_empty_column_list_clears_the_board() {
        let app = App::test_app();
        board_put(
            State(app.clone()),
            Json(BoardPutReq {
                columns: vec![column("todo", &[])],
            }),
        )
        .await;

        board_put(
            State(app.clone()),
            Json(BoardPutReq {
                columns: Vec::new(),
            }),
        )
        .await;

        // Deleting the last column is legal; the frontend falls back to a default column so
        // no clone is ever left without one.
        assert!(app.store.get().board_columns.is_empty());
    }

    // --- POST /api/activate (selection, and the layout that follows it) ---

    /// Selecting a clone is what brings it to the active layout. A preset activated while the
    /// operator was elsewhere never reached this clone, precisely so the fleet would not
    /// rebuild every Mutter session at once, so the switch is the only place left to apply it.
    #[tokio::test]
    async fn selecting_a_clone_pushes_the_active_layout_to_it() {
        let app = test_app();
        let (client, conn) = crate::mediaplane::accepted_conn("web-activate");
        app.media.insert_conn_for_test("w2", conn);
        app.store.mutate(|s| {
            for id in ["w1", "w2"] {
                s.hosts.push(wire::RmngClone {
                    id: id.into(),
                    host: id.into(),
                    managed: true,
                    ..Default::default()
                });
            }
            s.selected = Some("w1".into());
        });

        let _ = activate(
            State(app.clone()),
            Json(ActivateReq {
                id: Some("w2".into()),
            }),
        )
        .await;

        let got = crate::mediaplane::recv_now(&client).expect("the clone got a layout push");
        match serde_json::from_slice::<wire::socket::ServerMsg>(&got).unwrap() {
            wire::socket::ServerMsg::SetMonitors { monitors } => {
                assert_eq!(monitors, app.config().effective_monitors());
            }
            other => panic!("expected SetMonitors, got {other:?}"),
        }

        // Re-selecting the same clone is not a switch. Nothing changed, so nothing is pushed.
        let _ = activate(
            State(app.clone()),
            Json(ActivateReq {
                id: Some("w2".into()),
            }),
        )
        .await;
        assert!(
            crate::mediaplane::recv_now(&client).is_none(),
            "a no-op select pushed a layout"
        );
    }

    // --- GET /api/state (single-shot snapshot for the rmng CLI) ---

    // --- POST /api/clone (template clone: title + preset) ---

    #[tokio::test]
    async fn clone_plain_mode_registers_clone_op() {
        let app = test_app();
        let body = json!({ "plain": { "title": "encoder scratch", "message": "hi" } });
        let resp = clone(State(app.clone()), Json(body)).await.unwrap().0;
        assert_eq!(resp["ok"], true);
        let op: Operation = serde_json::from_value(resp["op"].clone()).unwrap();
        assert_eq!(op.kind, wire::OperationKind::Clone);
        assert!(app.store.get().operations.iter().any(|o| o.id == op.id));
    }

    #[tokio::test]
    async fn clone_plain_mode_rejects_unknown_preset() {
        let app = test_app();
        let body = json!({ "plain": { "title": "x" }, "preset": "nope" });
        let err = clone(State(app.clone()), Json(body)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("unknown preset"), "msg: {}", err.1);
    }

    #[tokio::test]
    async fn clone_requires_a_title() {
        let app = test_app();
        let body = json!({ "plain": { "title": "   " } });
        let err = clone(State(app.clone()), Json(body)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("plain.title is required"), "msg: {}", err.1);
    }

    #[tokio::test]
    async fn clone_requires_a_plain_body() {
        let app = test_app();
        // Retired modes (hostname / linear) and a bare body all land here now.
        for body in [
            json!({ "hostname": "w-x" }),
            json!({ "linear": { "ticket": "WE-142" } }),
            json!({}),
        ] {
            let err = clone(State(app.clone()), Json(body)).await.unwrap_err();
            assert_eq!(err.0, StatusCode::BAD_REQUEST);
            assert!(err.1.contains("{ plain: { title } }"), "msg: {}", err.1);
        }
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

    /// Deleting a pool must not strand the clones bound to it.
    ///
    /// Pools are replaced wholesale by `PUT /api/config`, so an omitted pool is a deletion.
    /// `claude::rotate_once` skips a group it cannot find, so a clone left naming a deleted pool
    /// is never rebalanced again — it freezes on its last account, silently, forever.
    #[test]
    fn deleting_a_pool_repoints_its_clones_at_auto() {
        let app = test_app();
        let pool = |n: &str| wire::CloneGroup {
            name: n.into(),
            accounts: vec![],
        };
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

        let by_id = |id: &str| {
            app.store
                .get()
                .hosts
                .into_iter()
                .find(|h| h.id == id)
                .unwrap()
        };
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

    /// Two managed clones on the rmng bridge, addressed the way Docker's IPAM addresses them.
    /// `pega-template` is the clone the fleet's images were committed from, so it is the one a
    /// stale key names.
    fn two_clones() -> App {
        let app = test_app();
        for (id, ip) in [("pega-template", "10.99.0.4"), ("pega-we-649", "10.99.0.6")] {
            app.store.mutate(|s| {
                s.hosts.push(wire::RmngClone {
                    id: id.into(),
                    host: id.into(),
                    managed: true,
                    local_ip: Some(ip.into()),
                    ..Default::default()
                })
            });
        }
        app
    }

    fn connect_info(ip: Option<std::net::IpAddr>) -> Option<ConnectInfo<std::net::SocketAddr>> {
        ip.map(|ip| ConnectInfo(std::net::SocketAddr::new(ip, 44444)))
    }

    /// Two clones on the rmng bridge, and the caller holds the wrong one's key while also
    /// naming the wrong one in its hostname header. That is the shape a clone born from an image
    /// that baked `pega-template`'s `/etc/environment` has for its whole life, and it is why
    /// neither of those is allowed to outrank the address the request arrived on.
    #[tokio::test]
    async fn the_address_outranks_anything_the_caller_says_about_itself() {
        let app = two_clones();
        let stale = app.clone_keys.mint("pega-template");
        let mut headers = HeaderMap::new();
        headers.insert("x-rmng-proxy-key", stale.parse().unwrap());
        headers.insert("x-rmng-clone", "pega-template".parse().unwrap());
        let peer = Some(std::net::IpAddr::from([10, 99, 0, 6]));

        assert_eq!(
            caller_clone(&app, &headers, peer).as_deref(),
            Some("pega-we-649")
        );
        let me = clone_self(State(app.clone()), connect_info(peer), headers.clone())
            .await
            .unwrap();
        assert_eq!(me.0.id, "pega-we-649");
    }

    /// An IPv4 peer on a dual-stack listener arrives mapped into IPv6, and the same clone has to
    /// come back.
    #[tokio::test]
    async fn a_v4_mapped_peer_resolves_to_the_same_clone() {
        let app = two_clones();
        let mapped = Some("::ffff:10.99.0.6".parse::<std::net::IpAddr>().unwrap());
        assert_eq!(
            caller_clone(&app, &HeaderMap::new(), mapped).as_deref(),
            Some("pega-we-649")
        );
    }

    /// An address that names no clone, which is every request from the operator's LAN, falls
    /// through to the headers rather than guessing.
    #[tokio::test]
    async fn an_address_off_the_bridge_falls_through_to_the_headers() {
        let app = two_clones();
        let lan = Some(std::net::IpAddr::from([10, 0, 0, 15]));
        let mut headers = HeaderMap::new();
        assert_eq!(caller_clone(&app, &headers, lan), None);
        headers.insert(
            "x-rmng-proxy-key",
            app.clone_keys.mint("pega-we-649").parse().unwrap(),
        );
        assert_eq!(
            caller_clone(&app, &headers, lan).as_deref(),
            Some("pega-we-649")
        );
    }

    /// Two rows on one address means the IP map is mid-refresh, so it answers nothing at all.
    #[tokio::test]
    async fn a_duplicated_address_is_not_an_identity() {
        let app = two_clones();
        app.store.mutate(|s| {
            for h in &mut s.hosts {
                h.local_ip = Some("10.99.0.6".into());
            }
        });
        let peer = Some(std::net::IpAddr::from([10, 99, 0, 6]));
        assert_eq!(caller_clone(&app, &HeaderMap::new(), peer), None);
    }

    #[tokio::test]
    async fn clone_self_from_an_unplaceable_caller_is_404_not_a_guess() {
        let app = two_clones();
        let err = clone_self(State(app.clone()), None, HeaderMap::new())
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert!(err.1.contains("not running inside"), "msg: {}", err.1);
    }

    /// The hostname alone proves nothing. An operator laptop that happens to be named after a
    /// clone sends no key, and must stay outside the fleet.
    #[tokio::test]
    async fn a_hostname_without_a_key_is_not_a_clone() {
        let app = two_clones();
        let mut headers = HeaderMap::new();
        headers.insert("x-rmng-clone", "pega-we-649".parse().unwrap());
        assert_eq!(caller_clone(&app, &headers, None), None);
    }

    /// An unmanaged host's name is not an identity either, so the key still decides. This is
    /// also the old-CLI path: no hostname header at all.
    #[tokio::test]
    async fn the_key_still_answers_when_the_hostname_names_nothing() {
        let app = two_clones();
        let key = app.clone_keys.mint("pega-we-649");
        let mut headers = HeaderMap::new();
        headers.insert("x-rmng-proxy-key", key.parse().unwrap());
        assert_eq!(
            caller_clone(&app, &headers, None).as_deref(),
            Some("pega-we-649")
        );
        headers.insert("x-rmng-clone", "someones-laptop".parse().unwrap());
        assert_eq!(
            caller_clone(&app, &headers, None).as_deref(),
            Some("pega-we-649")
        );
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
        assert!(
            v.get("detach").is_none(),
            "detach:false must be omitted from the wire"
        );
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
    fn parse_env_lines_keeps_assignments_and_unquoted_values() {
        // Real `systemctl --user show-environment` shape: one KEY=VALUE per line, values may
        // themselves contain `=` (DBUS address) and need no quoting. Blank lines and any
        // non-assignment noise are dropped.
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
    fn a_shell_quoted_value_arrives_as_the_value_itself() {
        // PER-58. Every form below was measured against `systemctl --user show-environment`
        // inside a clone, not taken from the shell grammar.
        let out = "\
ANTHROPIC_MODEL=$'opus[1m]'
SPACED=$'a b'
APOSTROPHE=$'it\\'s'
BACKSLASH=$'back\\\\slash'
TABBED=$'tab\\there'
BELL=$'bell\\a'
CTRL=$'ctrl\\001byte'
DOLLAR=$'dollar$x'
UTF8=café
PLAIN=plainvalue
";
        assert_eq!(
            parse_env_lines(out),
            vec![
                "ANTHROPIC_MODEL=opus[1m]".to_string(),
                "SPACED=a b".to_string(),
                "APOSTROPHE=it's".to_string(),
                "BACKSLASH=back\\slash".to_string(),
                "TABBED=tab\there".to_string(),
                "BELL=bell\u{7}".to_string(),
                "CTRL=ctrl\u{1}byte".to_string(),
                "DOLLAR=dollar$x".to_string(),
                "UTF8=café".to_string(),
                "PLAIN=plainvalue".to_string(),
            ]
        );
    }

    #[test]
    fn an_octal_escape_is_one_byte_not_one_character() {
        // `é` is two bytes, and systemd escapes each on its own. Decoding per character would
        // hand the clone two characters of mojibake instead.
        assert_eq!(unquote_shell_value("$'caf\\303\\251'"), "café");
        assert_eq!(unquote_shell_value("$'caf\\xc3\\xa9'"), "café");
    }

    #[test]
    fn a_value_that_only_looks_quoted_is_left_alone() {
        // The opening `$'` is what marks systemd's quoting. Anything else is a value.
        assert_eq!(unquote_shell_value("plain"), "plain");
        assert_eq!(unquote_shell_value("'quoted'"), "'quoted'");
        assert_eq!(unquote_shell_value("$'unterminated"), "$'unterminated");
        assert_eq!(unquote_shell_value(""), "");
    }

    #[test]
    fn merge_env_lets_caller_override_and_appends_new() {
        let mut base = vec![
            "WAYLAND_DISPLAY=wayland-0".to_string(),
            "PATH=/session/bin".to_string(),
            "XDG_RUNTIME_DIR=/run/user/1000".to_string(),
        ];
        // Caller overrides PATH and adds a brand-new key; the untouched session vars remain.
        merge_env(
            &mut base,
            &["PATH=/caller/bin".to_string(), "FOO=1".to_string()],
        );
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
    // key and never calls Linear, so what is worth pinning is narrow: whether the headers
    // and the bytes arrive as they left.

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

    /// The bytes and every header reach the target unchanged. A real PUT goes to Google, so
    /// this drives `relay_request` directly against a stub that answers with what it received,
    /// which is the way to observe the forwarding at all.
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
                let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                    .await
                    .unwrap();
                Json(json!({ "headers": seen, "len": body.len() }))
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let headers =
            relay_headers(r#"[{"key":"content-type","value":"image/png"},{"key":"x-goog-content-length-range","value":"0,8"}]"#)
                .unwrap();
        let bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let seen: serde_json::Value = relay_request(
            &reqwest::Client::new(),
            &format!("http://{addr}/put"),
            &headers,
        )
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

    // --- GET /api/linear/asset ---
    //
    // The read half. A Linear `assetUrl` answers an unauthenticated GET with 401 and no
    // redirect, and `uploads.linear.app` leaves `authorization` out of its CORS allow-list, so
    // neither an `<img>` nor a `fetch` in the page can read one. This route holds a key and
    // hands the bytes back same-origin.

    /// An `assetUrl` names a file, not a team, so there is no workspace to pick a key from.
    /// Each configured key is tried until one is allowed to read it, the way `fetch_issue_any`
    /// answers the same question for issues. Driven against a stub, as the relay's forwarding
    /// test is, so the loop is observable at all.
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
                        return (
                            StatusCode::UNAUTHORIZED,
                            [(header::CONTENT_TYPE, "application/json")],
                            b"{\"error\":\"unauthorized\"}".to_vec(),
                        );
                    }
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "image/png")],
                        served,
                    )
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let url = format!("http://{addr}/asset");

        // The second key is the one with access. The bytes and the upstream's own content
        // type come back, and both keys were spent getting there.
        let keys = vec!["first".to_string(), "second".to_string()];
        let resp = fetch_asset(&app.http, &url, &keys).await.unwrap();
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(resp.bytes().await.unwrap().to_vec(), png);
        assert_eq!(tries.load(Ordering::SeqCst), 2);

        // No key has access: the answer reports what Linear said about the last one tried.
        let (status, text) = fetch_asset(&app.http, &url, &["first".into(), "third".into()])
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(text.contains("401"), "{text}");

        // No key configured at all says so, instead of reporting a network failure.
        let (_, text) = fetch_asset(&app.http, &url, &[]).await.unwrap_err();
        assert!(text.contains("no preset has a Linear API key"), "{text}");
    }

    /// Both Linear routes, against Linear, in the order the browser drives them.
    #[test]
    fn the_asset_proxy_takes_every_distinct_configured_key_in_config_order() {
        let preset = |key: &str| wire::Preset {
            linear_key: key.into(),
            ..Default::default()
        };
        let cfg = wire::AppConfig {
            presets: vec![
                preset(""),
                preset("K2"),
                preset("K1"),
                preset("K2"),
                preset("  "),
            ],
            ..Default::default()
        };
        assert_eq!(linear_keys(&cfg), vec!["K2".to_string(), "K1".to_string()]);
        assert!(linear_keys(&wire::AppConfig::default()).is_empty());
    }

    /// One asset through the whole read path: the type Linear declared reaches the browser as
    /// the type Linear declared, and the bytes arrive intact.
    #[tokio::test]
    async fn an_asset_is_served_as_the_type_linear_declared() {
        let png = b"\x89PNG\r\n\x1a\nrmng".to_vec();
        let served = png.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stub = Router::new().route(
            "/png",
            get(move || {
                let body = served.clone();
                async move { ([(header::CONTENT_TYPE, "image/png")], body) }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let ok = asset_response(
            &app.http,
            &format!("http://{addr}/png"),
            &["k".to_string()],
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(ok.headers()[header::CONTENT_TYPE], "image/png");
        let bytes = axum::body::to_bytes(ok.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.to_vec(), png);
    }

    /// The deadline bounds the request, not each key attempt.
    ///
    /// Per attempt it is a multiplier: two keys against a target that never answers measured
    /// 40.002s of held connection at a 20-second per-attempt timeout, and five presets would
    /// be 100s. Three keys here, one budget, and the whole thing has to end inside a budget
    /// and a bit rather than three of them.
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
        let (status, text) = asset_response(&app.http, &format!("http://{addr}/x"), &keys, budget)
            .await
            .unwrap_err();
        let took = started.elapsed();

        println!("three keys, one {budget:?} budget, stalled target: {took:?} -> {status} {text}");
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{text}");
        assert!(text.contains("within 400ms"), "{text}");
        // Three keys times the budget would be 1.2s. Generous headroom, and still nowhere
        // near what a per-attempt bound would take.
        assert!(
            took < budget * 2,
            "one budget for three keys, took {took:?}"
        );
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
                    let stream = futures::stream::once(async move {
                        Ok::<_, std::io::Error>(vec![b'a'; 64 * 1024])
                    })
                    .chain(futures::stream::once(async move {
                        let rx = gate.lock().await.take().unwrap();
                        let _ = rx.await;
                        Ok::<_, std::io::Error>(vec![b'b'; 16])
                    }));
                    (
                        [(header::CONTENT_TYPE, "image/png")],
                        axum::body::Body::from_stream(stream),
                    )
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let (client, upstream) = (app.http.clone(), format!("http://{addr}/slow"));
        let router = Router::new().route(
            "/asset",
            get(move || {
                let (client, upstream) = (client.clone(), upstream.clone());
                async move {
                    asset_response(
                        &client,
                        &upstream,
                        &["k".to_string()],
                        Duration::from_secs(20),
                    )
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
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    axum::body::Body::from_stream(stream),
                )
            }),
        );
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let app = test_app();
        let keys = vec!["k".to_string()];
        let budget = Duration::from_secs(30);

        let (status, text) = asset_response(
            &app.http,
            &format!("http://{raw_addr}/declared"),
            &keys,
            budget,
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(text.contains("32MB"), "{text}");

        // No length to go on: the response starts, and the body stops at the cap.
        let ok = asset_response(&app.http, &format!("http://{addr}/chunked"), &keys, budget)
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let cut = axum::body::to_bytes(ok.into_body(), usize::MAX).await;
        assert!(cut.is_err(), "an over-cap body must not read to completion");
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

        assert!(
            boot.starts_with("boot-"),
            "dev runs get a per-boot id, got {boot}"
        );
        assert_eq!(
            app.build_id(),
            boot,
            "must not change while the process lives"
        );

        // An image built without `GIT_SHA` labels an empty revision; keep the boot id rather
        // than publish an empty identity every client would compare equal.
        app.set_build_id("");
        assert_eq!(app.build_id(), boot);

        app.set_build_id("2ae7f50");
        assert_eq!(app.build_id(), "2ae7f50");
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
