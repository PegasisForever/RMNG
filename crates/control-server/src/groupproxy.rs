//! `rmng-cliproxy` — the group-proxy sidecar container: the `/cc` router, the per-group
//! CLIProxyAPI supervisor, and the admin-forward surface the control-server reaches them
//! through.
//!
//! # Why this is its own container
//!
//! Every agent request from every clone used to run
//! `clone → control-server /cc → loopback CLIProxyAPI`. Both hops lived in the
//! control-server process, so **updating the control-server killed every in-flight agent
//! turn in the fleet** — and a control-server update is a routine, frequent thing while
//! clones are doing hours-long work. Moving only the CLIProxyAPI processes out would not have
//! helped: the router is what the clones dial, so recreating the control-server container
//! would still drop the connection mid-stream.
//!
//! So both hops moved. The control-server ensures a long-lived container named
//! [`CONTAINER`], running THIS SAME IMAGE under the `group-proxy` argv subcommand (dispatched
//! in `main.rs`, mirroring `self-upgrade`). Clones dial it directly at
//! `http://rmng-cliproxy:9010/cc`; the control-server is out of the data path entirely.
//!
//! # Lifecycle: deliberately NOT rolled by a control-server update
//!
//! [`crate::docker::DockerCtl::ensure_group_proxy`] is create-if-absent / start-if-stopped and
//! **nothing else**. Unlike `ensure_build_infra` it does NOT recreate on image drift: after a
//! control-server update the sidecar keeps running the older image, which is precisely the
//! point — the operator rolls it forward with `POST /api/groupproxy/restart` (a button in
//! Settings) when clones are idle. `restart: unless-stopped` brings it back after a host
//! reboot. `GET /api/groupproxy` surfaces its running image + revision so "behind" is visible.
//!
//! # Inputs: the shared `/data` volume, not IPC
//!
//! The sidecar mounts the SAME `/data` volume as the control-server (discovered from the
//! control-server's own mounts, so the volume name is never hardcoded) and reads:
//!   - `config.json` → `groups` (which instances to supervise) — watched via the same
//!     `notify` recommended-watcher pattern as [`crate::state::spawn_watcher`];
//!   - `data/state.json` → each clone's group binding — same watcher;
//!   - `data/cliproxy-instances.json` → per-group ports/keys, per-clone router bearers, and
//!     the shared admin secret — polled on [`SECRETS_POLL`] rather than watched, because it is
//!     rewritten atomically (temp+rename) by the control-server on every clone create, and a
//!     1 s poll of a few-KB file is simpler than debouncing rename events for the one file
//!     whose staleness costs a retryable 401.
//!
//! The sidecar NEVER writes any of them: `CliProxyManager` is opened read-only, so the
//! control-server stays the single writer of the secrets file (see [`crate::cliproxy`]).
//!
//! # Token accounting across the boundary
//!
//! `TokenBus` — persistence (`clone-tokens.json`), the SSE projection, and the `capture_epoch`
//! lifecycle guard — stays owned solely by the control-server; two writers of that file would
//! corrupt it. The observer runs here and its increments ride [`RemoteSink`]: coalesced per
//! `(clone, epoch)`, flushed to the control-server's `POST /internal/tokens` on [`FLUSH_EVERY`],
//! and held in a bounded in-memory buffer across a failure. That buffer is the feature: the
//! window it survives is exactly the control-server upgrade this whole split exists to make
//! non-disruptive. The epoch itself crosses as data — [`crate::tokens::read_lifecycle_epochs`]
//! reads it from the shared `clone-tokens.json` — and the control-server re-validates every
//! arriving delta, so a stale read costs at most a dropped delta, never a misattributed one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::{Path as AxPath, Request, State},
    http::{HeaderName, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use futures::StreamExt;
use wire::AppConfig;

use crate::cliproxy::{CliProxyManager, SupervisorCtx};
use crate::tokens::{ResponseObserver, TokenDelta, UsageSink};

// --- the shared surface both binaries agree on -----------------------------------------

/// The sidecar container's name AND its DNS alias on the `rmng` bridge. Clones bake this into
/// `ANTHROPIC_BASE_URL`, so it must never change without a fleet-wide reconcile.
pub const CONTAINER: &str = "rmng-cliproxy";
/// The port the sidecar serves `/cc` + `/admin` on. Deliberately clear of the control-server's
/// 9000/9001/9005 and of [`crate::cliproxy`]'s 9100+ instance range. Never published to the
/// host — bridge-internal only.
pub const PORT: u16 = 9010;
/// Header carrying the shared control-server ↔ group-proxy admin secret. A clone holds only a
/// per-clone router bearer, so it can never satisfy this.
pub const ADMIN_HEADER: &str = "x-rmng-admin-key";

/// The clone-facing base for Claude Code's `ANTHROPIC_BASE_URL` (it appends `/v1/messages`,
/// `/v1/models`). Pure — see the URL tests.
pub fn cc_base_url() -> String {
    format!("http://{CONTAINER}:{PORT}/cc")
}

/// The clone-facing OpenAI-compatible base for the generated Codex/OpenCode provider configs.
pub fn cc_v1_base_url() -> String {
    format!("{}/v1", cc_base_url())
}

/// The control-server's URL for one group instance's management API, forwarded by the sidecar:
/// `…/admin/<group>/mgmt<path_and_query>` → `http://127.0.0.1:<port>/v0/management<…>` with the
/// group's `X-Management-Key` attached on the far side. `path_and_query` starts with `/`.
pub fn admin_mgmt_url(group: &str, path_and_query: &str) -> String {
    format!(
        "http://{CONTAINER}:{PORT}/admin/{}/mgmt{path_and_query}",
        urlencode_segment(group)
    )
}

/// The control-server's URL for one group instance's `/v1/models` catalog, forwarded by the
/// sidecar with that instance's inbound key attached (see [`crate::cliproxy::group_catalog`]).
pub fn admin_catalog_url(group: &str) -> String {
    format!(
        "http://{CONTAINER}:{PORT}/admin/{}/v1/models",
        urlencode_segment(group)
    )
}

/// Percent-encode a path segment. Group names are already restricted to `[A-Za-z0-9._-]` by
/// `cliproxy::safe_group`, so this is belt-and-braces against a hand-edited config reaching the
/// URL builder — the sidecar re-validates `safe_group` on arrival anyway.
fn urlencode_segment(s: &str) -> String {
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

// --- cadences ---------------------------------------------------------------------------

/// How often the sidecar re-reads `cliproxy-instances.json` (new group meta, new clone router
/// keys, the admin secret). A miss here is a retryable 401/503 for one request, so a short poll
/// beats watching a file the control-server rewrites wholesale.
const SECRETS_POLL: Duration = Duration::from_secs(1);
/// How often the sidecar re-reads `clone-tokens.json` for each clone's active lifecycle epoch.
/// Slower than [`SECRETS_POLL`] because a stale epoch costs only a dropped delta (the
/// control-server re-validates), and archive/unarchive is rare.
const EPOCH_POLL: Duration = Duration::from_secs(5);
/// How often buffered token deltas are POSTed to the control-server. Short enough that the UI's
/// token counters feel live, long enough that a busy stream is one request rather than dozens.
const FLUSH_EVERY: Duration = Duration::from_millis(750);
/// Upper bound on distinct `(clone, epoch)` entries held while the control-server is
/// unreachable. Deltas for one pair coalesce in place, so this bounds MEMORY, not throughput:
/// only a fleet with this many clones all mid-request during an upgrade can overflow, and the
/// overflow drops new pairs (never corrupting an existing count).
const MAX_PENDING: usize = 4096;
/// Request timeout for one delta flush. Kept well under [`FLUSH_EVERY`] × a few so a
/// control-server that is down (connection refused is instant) or wedged never stalls the
/// flusher into unbounded lag.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

// --- shared state -----------------------------------------------------------------------

/// The sidecar's whole world: read-only views of the shared `/data` volume plus the HTTP
/// clients. Deliberately NOT [`crate::app::App`] — there is no Docker client, media plane,
/// state store, or chat here, and depending on `App` would drag all of that into a process
/// whose entire job is to forward bytes.
#[derive(Clone)]
pub struct ProxyApp {
    pub cliproxy: Arc<CliProxyManager>,
    /// Live `config.json` (the group list + `data_dir`), refreshed by the config watcher.
    pub cfg: Arc<RwLock<AppConfig>>,
    /// Live `state.json` clone rows — only `id`, `group`, and `archived` are read. Held as a
    /// plain map rather than a `StateStore` because the sidecar must never write state.
    pub clones: Arc<RwLock<HashMap<String, CloneRow>>>,
    /// Per-clone active token lifecycle epoch, from the shared `clone-tokens.json`.
    pub epochs: Arc<RwLock<HashMap<String, u64>>>,
    /// Transparent upstream transport for BOTH the `/cc` router and the admin forwards:
    /// redirects stay client-visible rather than being followed here, so the upstream method
    /// and response are reproduced exactly. (The token-delta flush uses [`RemoteSink`]'s own
    /// plain client — it wants normal redirect handling and a per-request timeout.)
    pub proxy_http: reqwest::Client,
    pub tokens: Arc<RemoteSink>,
}

/// The only three facts the router needs about a clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneRow {
    pub group: String,
    pub archived: bool,
}

impl ProxyApp {
    fn data_dir(&self) -> String {
        self.cfg.read().unwrap().data_dir.clone()
    }
}

// --- token delta forwarding --------------------------------------------------------------

/// The out-of-process [`UsageSink`]: coalesce deltas per `(clone, epoch)` and ship them to the
/// control-server's `POST /internal/tokens`. Submission is lock-and-return — never any I/O on
/// the proxy's streaming path — and the flusher task drains the buffer on [`FLUSH_EVERY`].
///
/// On a failed flush the batch is folded BACK into the buffer (still coalesced, so a long
/// outage costs bounded memory rather than a growing queue) and retried on the next tick. That
/// is what carries accounting through a control-server upgrade.
pub struct RemoteSink {
    pending: Mutex<HashMap<(String, u64), TokenDelta>>,
    /// Count of `(clone, epoch)` pairs dropped because the buffer was full, so the operator
    /// sees under-counting in the log instead of silently.
    dropped: Mutex<u64>,
    control_url: String,
    admin_secret: Arc<RwLock<Option<String>>>,
    http: reqwest::Client,
}

impl UsageSink for RemoteSink {
    fn submit(&self, delta: TokenDelta) {
        self.enqueue(delta);
    }
}

impl RemoteSink {
    pub fn new(
        control_url: String,
        admin_secret: Arc<RwLock<Option<String>>>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            dropped: Mutex::new(0),
            control_url,
            admin_secret,
            http,
        }
    }

    /// Fold one delta into the pending buffer. Pure bookkeeping (no I/O, no await) so it is
    /// safe to call from the body-stream map closure. Returns whether it was accepted, which is
    /// what the bound test asserts.
    pub fn enqueue(&self, delta: TokenDelta) -> bool {
        let key = (delta.host_id.clone(), delta.epoch);
        let mut pending = self.pending.lock().unwrap();
        if let Some(existing) = pending.get_mut(&key) {
            existing.merge(&delta);
            return true;
        }
        // A full buffer refuses only NEW pairs: an in-flight clone's running total keeps
        // accumulating correctly, and the dropped pairs are counted + logged.
        if pending.len() >= MAX_PENDING {
            drop(pending);
            let mut dropped = self.dropped.lock().unwrap();
            *dropped = dropped.saturating_add(1);
            return false;
        }
        pending.insert(key, delta);
        true
    }

    /// Number of `(clone, epoch)` pairs currently buffered — the observable the bound test
    /// asserts against.
    #[cfg(test)]
    pub fn pending_len(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    fn take_batch(&self) -> Vec<TokenDelta> {
        let mut pending = self.pending.lock().unwrap();
        pending.drain().map(|(_, delta)| delta).collect()
    }

    /// Put a failed batch back, merging into anything that accumulated while it was in flight.
    /// Re-uses [`Self::enqueue`], so the same bound applies to the retry path.
    fn requeue(&self, batch: Vec<TokenDelta>) {
        for delta in batch {
            self.enqueue(delta);
        }
    }

    /// Flush forever. Never returns; every failure is logged once per transition so a
    /// control-server that is down for an upgrade doesn't spam a line per tick.
    pub async fn run_flusher(self: Arc<Self>) {
        let mut failing = false;
        loop {
            tokio::time::sleep(FLUSH_EVERY).await;
            let batch = self.take_batch();
            if batch.is_empty() {
                continue;
            }
            let Some(secret) = self.admin_secret.read().unwrap().clone() else {
                // The control-server hasn't minted the shared secret yet (first boot). Hold the
                // deltas — it will, and the buffer is bounded.
                self.requeue(batch);
                continue;
            };
            let count = batch.len();
            let sent = self
                .http
                .post(&self.control_url)
                .header(ADMIN_HEADER, secret)
                .timeout(FLUSH_TIMEOUT)
                .json(&batch)
                .send()
                .await;
            match sent {
                Ok(resp) if resp.status().is_success() => {
                    if failing {
                        tracing::info!(
                            target: "groupproxy",
                            "token delta flush recovered ({count} buffered batches applied)"
                        );
                        failing = false;
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    // A 4xx is a permanent rejection (bad secret / malformed body): retrying
                    // forever would just pin the buffer, so drop it and say so loudly.
                    if status.is_client_error() {
                        tracing::error!(
                            target: "groupproxy",
                            "control-server rejected {count} token deltas with {status} — dropping them"
                        );
                    } else {
                        self.requeue(batch);
                        if !failing {
                            tracing::warn!(target: "groupproxy", "token delta flush got {status}; buffering");
                            failing = true;
                        }
                    }
                }
                Err(e) => {
                    self.requeue(batch);
                    if !failing {
                        tracing::warn!(
                            target: "groupproxy",
                            "token delta flush failed ({e}); buffering until the control-server is back \
                             (this is the control-server-upgrade window)"
                        );
                        failing = true;
                    }
                }
            }
            let dropped = *self.dropped.lock().unwrap();
            if dropped > 0 && !failing {
                tracing::warn!(
                    target: "groupproxy",
                    "{dropped} clone/epoch token-delta pairs were dropped while the buffer was full"
                );
                *self.dropped.lock().unwrap() = 0;
            }
        }
    }
}

// --- the /cc router (moved verbatim from web.rs) -----------------------------------------

/// Headers we never forward verbatim in either direction: framing/connection headers are
/// recomputed per hop. `Connection` can nominate additional hop-by-hop headers, so collect its
/// comma-separated tokens in addition to the RFC-defined set. `authorization` is handled
/// separately by the router (dropped inbound, replaced with the instance's inbound key).
pub fn hop_by_hop_headers(headers: &axum::http::HeaderMap) -> std::collections::HashSet<HeaderName> {
    let mut names = [
        header::HOST,
        header::CONNECTION,
        header::CONTENT_LENGTH,
        header::TRANSFER_ENCODING,
        HeaderName::from_static("keep-alive"),
        header::TE,
        header::TRAILER,
        header::UPGRADE,
        HeaderName::from_static("proxy-authenticate"),
        HeaderName::from_static("proxy-authorization"),
        HeaderName::from_static("proxy-connection"),
    ]
    .into_iter()
    .collect::<std::collections::HashSet<_>>();
    for value in headers.get_all(header::CONNECTION) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
                names.insert(name);
            }
        }
    }
    names
}

/// `ANY /cc/*rest` — reverse-proxy a clone's agent traffic (Claude Code, Codex, OpenCode)
/// to its bound group's CLIProxyAPI instance on loopback IN THIS CONTAINER.
///
/// 1. `Authorization: Bearer <per-clone key>` → clone id (unknown/missing → 401).
/// 2. clone id → `clone.group` (none → 409 "clone has no group").
/// 3. group → instance loopback port + inbound key (missing/booting → 503; the agent retries).
/// 4. Forward the method + `*rest` path + query to `http://127.0.0.1:<port>/<rest>`, copying
///    every non-hop-by-hop header except `Authorization`, SETTING `Authorization: Bearer
///    <inbound_key>` and `X-Session-ID: <host_id>` (per-clone session stickiness), STREAMING
///    both request and response bodies so SSE (`text/event-stream`) is never buffered.
/// 5. Attach the token observer to cloned response chunks (successful, uncompressed, accountable
///    routes only); its increments go to the control-server via [`RemoteSink`].
async fn cc_proxy(State(app): State<ProxyApp>, req: Request) -> Response {
    let deny = |code: StatusCode, msg: &str| (code, msg.to_string()).into_response();

    // 1. Per-clone bearer key → clone id.
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(host_id) = token.and_then(|t| app.cliproxy.clone_for_token(t)) else {
        return deny(
            StatusCode::UNAUTHORIZED,
            "unknown or missing router bearer key",
        );
    };

    // 2. Clone → active group binding.
    let Some(host) = app.clones.read().unwrap().get(&host_id).cloned() else {
        return deny(StatusCode::UNAUTHORIZED, "unknown clone");
    };
    if host.archived {
        return deny(
            StatusCode::CONFLICT,
            "clone is archived; unarchive it before using inference",
        );
    }
    // Blank is unreachable once the control-server's `normalize_clone_groups` has run (boot +
    // every reconciler pass), but a hand-edited state.json can be blank for the window before
    // this process picks it up — 409 rather than routing to an instance that doesn't exist.
    let group = host.group;
    if group.is_empty() {
        return deny(
            StatusCode::CONFLICT,
            "clone has no group (bind one in Settings before running an agent)",
        );
    }
    // Capture before the upstream request can wait on headers, so a response from this request
    // cannot be attributed to a later lifecycle. Read from the shared clone-tokens.json snapshot;
    // absent ⇒ no active managed record ⇒ no observer, same as `TokenBus::capture_epoch`'s None.
    let capture_epoch = app.epochs.read().unwrap().get(&host_id).copied();

    // 3. Group → loopback instance.
    let (Some(port), Some(inbound_key)) = (
        app.cliproxy.port_for(&group),
        app.cliproxy.inbound_key_for(&group),
    ) else {
        return deny(
            StatusCode::SERVICE_UNAVAILABLE,
            "group instance unavailable (still starting) — retry",
        );
    };

    // 4. Build + forward the streamed request.
    let (parts, body) = req.into_parts();
    let path = parts.uri.path();
    let rest = path
        .strip_prefix("/cc")
        .filter(|s| !s.is_empty())
        .unwrap_or("/")
        .to_string();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!("http://127.0.0.1:{port}{rest}{query}");

    let request_hop_headers = hop_by_hop_headers(&parts.headers);
    let mut headers = reqwest::header::HeaderMap::new();
    for (k, v) in parts.headers.iter() {
        if request_hop_headers.contains(k) || k == header::AUTHORIZATION {
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
        .proxy_http
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

    // 5. Stream the response back (status + headers + body), unbuffered. When the response is
    // successful, uncompressed, and a supported client-facing route, attach a local observer to
    // cloned chunks. It never mutates body bytes/errors or response headers, and it does no I/O
    // (the RemoteSink's submit is a lock-and-return; the flusher owns the network).
    let status = resp.status();
    let streaming = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
    let encoded = resp
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"));
    let mut observer = if status.is_success() && !encoded {
        capture_epoch.and_then(|epoch| {
            ResponseObserver::new(
                app.tokens.clone() as Arc<dyn UsageSink>,
                host_id.clone(),
                epoch,
                &rest,
                streaming,
            )
        })
    } else {
        None
    };
    let response_hop_headers = hop_by_hop_headers(resp.headers());
    let mut builder = Response::builder().status(status);
    for (k, v) in resp.headers().iter() {
        if response_hop_headers.contains(k) {
            continue;
        }
        builder = builder.header(k.clone(), v.clone());
    }
    let stream = resp.bytes_stream().map(move |chunk| {
        if let (Some(observer), Ok(bytes)) = (observer.as_mut(), &chunk) {
            observer.feed(bytes);
        }
        chunk
    });
    builder.body(Body::from_stream(stream)).unwrap_or_else(|e| {
        tracing::error!(target: "router", "building proxied response: {e}");
        StatusCode::BAD_GATEWAY.into_response()
    })
}

// --- the admin forward surface -----------------------------------------------------------

/// Whether a request carries the shared admin secret. Two failure modes both deny:
/// a mismatched header, and a secret the control-server hasn't minted yet (`None` ⇒ deny, never
/// fall open). Pure over `(expected, presented)` so both are unit-tested.
pub fn admin_authorized(expected: Option<&str>, presented: Option<&str>) -> bool {
    match (expected, presented) {
        (Some(expected), Some(presented)) if !expected.is_empty() => expected == presented,
        _ => false,
    }
}

/// `ANY /admin/:group/mgmt/*rest` — forward the control-server's onboarding/management calls to
/// the group's loopback instance, attaching that instance's plaintext `X-Management-Key`. The
/// management APIs are unreachable from outside this container now, so this is the ONLY way the
/// control-server drives OAuth enrollment and credential deletion.
///
/// Authenticated by [`ADMIN_HEADER`] alone: a clone holds only a per-clone router bearer, which
/// never satisfies this, so the management surface stays out of clone reach.
async fn admin_mgmt(
    State(app): State<ProxyApp>,
    AxPath((group, rest)): AxPath<(String, String)>,
    req: Request,
) -> Response {
    forward_to_instance(app, group, format!("/v0/management/{rest}"), req, AuthMode::Management).await
}

/// `GET /admin/:group/v1/models` — forward the group instance's model catalog, attaching that
/// instance's INBOUND key (the catalog is a data-plane route, not a management one). Feeds
/// `cliproxy::group_catalog`, which shapes each clone's Codex/OpenCode model list and Claude
/// Code's default model.
async fn admin_catalog(
    State(app): State<ProxyApp>,
    AxPath(group): AxPath<String>,
    req: Request,
) -> Response {
    forward_to_instance(app, group, "/v1/models".to_string(), req, AuthMode::Inbound).await
}

/// Which of a group instance's two credentials a forwarded request presents.
enum AuthMode {
    /// `X-Management-Key: <mgmt_secret>` — the management API.
    Management,
    /// `Authorization: Bearer <inbound_key>` — the data-plane API (`/v1/models`).
    Inbound,
}

async fn forward_to_instance(
    app: ProxyApp,
    group: String,
    upstream_path: String,
    req: Request,
    auth: AuthMode,
) -> Response {
    let deny = |code: StatusCode, msg: &str| (code, msg.to_string()).into_response();

    let presented = req
        .headers()
        .get(ADMIN_HEADER)
        .and_then(|v| v.to_str().ok());
    if !admin_authorized(app.cliproxy.admin_secret().as_deref(), presented) {
        return deny(StatusCode::UNAUTHORIZED, "admin key required");
    }
    // Re-validate even though the control-server only ever sends configured names: `group` lands
    // in a filesystem-derived instance lookup, and this is the trust boundary.
    if !crate::cliproxy::safe_group(&group) {
        return deny(StatusCode::BAD_REQUEST, "invalid group name");
    }

    let (Some(port), Some(inbound_key), Some((_, mgmt_secret))) = (
        app.cliproxy.port_for(&group),
        app.cliproxy.inbound_key_for(&group),
        app.cliproxy.management(&group),
    ) else {
        return deny(StatusCode::SERVICE_UNAVAILABLE, "group instance unavailable");
    };

    let (parts, body) = req.into_parts();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!("http://127.0.0.1:{port}{upstream_path}{query}");

    let hop = hop_by_hop_headers(&parts.headers);
    let mut headers = reqwest::header::HeaderMap::new();
    for (k, v) in parts.headers.iter() {
        // The admin key is ours, not the instance's; never leak it upstream.
        if hop.contains(k) || k == header::AUTHORIZATION || k.as_str() == ADMIN_HEADER {
            continue;
        }
        headers.insert(k.clone(), v.clone());
    }
    match auth {
        AuthMode::Management => {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&mgmt_secret) {
                headers.insert(HeaderName::from_static("x-management-key"), val);
            }
        }
        AuthMode::Inbound => {
            if let Ok(val) =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {inbound_key}"))
            {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }
    }

    let upstream_body = reqwest::Body::wrap_stream(body.into_data_stream());
    let resp = match app
        .proxy_http
        .request(parts.method, &url)
        .headers(headers)
        .body(upstream_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "groupproxy", "admin forward {url}: {e}");
            return deny(StatusCode::BAD_GATEWAY, "group instance request failed");
        }
    };
    let mut builder = Response::builder().status(resp.status());
    let response_hop = hop_by_hop_headers(resp.headers());
    for (k, v) in resp.headers().iter() {
        if response_hop.contains(k) {
            continue;
        }
        builder = builder.header(k.clone(), v.clone());
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// `GET /health` — unauthenticated liveness (no secrets, no group data). Lets the control-server
/// answer "is the group proxy actually serving?" for the Settings panel without a docker exec.
async fn health() -> Response {
    (StatusCode::OK, "ok").into_response()
}

pub fn router(app: ProxyApp) -> Router {
    Router::new()
        .route("/cc/*rest", any(cc_proxy))
        .route("/admin/:group/mgmt/*rest", any(admin_mgmt))
        .route("/admin/:group/v1/models", any(admin_catalog))
        .route("/health", axum::routing::get(health))
        .with_state(app)
}

// --- watchers ---------------------------------------------------------------------------

/// Re-read `config.json` + `data/state.json` from the shared volume on every filesystem event,
/// reusing [`crate::state::spawn_watcher`]'s `notify` recommended-watcher + 150 ms debounce
/// shape. Both files are written atomically (temp + rename) by the control-server, so a
/// directory watch is what sees them; a rename delivers an event for the directory, not the
/// replaced inode.
///
/// The initial read happens on the caller's thread before the watcher starts, so the router
/// never serves a request against an empty clone map.
fn spawn_input_watcher(app: ProxyApp) {
    use notify::{Event, RecursiveMode, Watcher};

    reload_config(&app);
    reload_clones(&app);

    // config.json is CWD-relative (WORKDIR /data), data/state.json lives under `data_dir`.
    let config_dir = crate::config::config_path()
        .parent()
        .map(Path::to_path_buf)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."));
    let state_dir = PathBuf::from(app.data_dir());

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(target: "groupproxy", "input watch disabled: {e} — config/state changes will NOT be picked up");
                return;
            }
        };
        for dir in [&config_dir, &state_dir] {
            // The sidecar can win the race to start on a fresh volume, before the
            // control-server has created `data/`. `notify` can only watch a path that
            // exists, and a failed watch here is PERMANENT — the clone→group map would
            // then never refresh for the life of the container, and every new clone's
            // agent would 401. Create the directory rather than warn about it; we only
            // ever read from it, so an empty dir is harmless if something else owns it.
            if !dir.exists() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                tracing::error!(
                    target: "groupproxy",
                    "watching {} failed: {e} — changes there will NOT be picked up until this \
                     container restarts",
                    dir.display()
                );
            }
        }
        tracing::info!(
            target: "groupproxy",
            "watching {} + {} for config/state changes",
            config_dir.display(),
            state_dir.display()
        );
        loop {
            if rx.recv().is_err() {
                break;
            }
            while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
            reload_config(&app);
            reload_clones(&app);
        }
    });
}

fn reload_config(app: &ProxyApp) {
    match crate::config::load_read_only() {
        Ok(cfg) => *app.cfg.write().unwrap() = cfg,
        Err(e) => tracing::warn!(target: "groupproxy", "reloading config.json: {e:#} (keeping the previous one)"),
    }
}

/// Project `state.json` down to the router's three fields. Read directly (not through
/// [`crate::state::StateStore`]) because that type owns a persist path and an SSE bus, neither
/// of which this process may have — the sidecar must never write `state.json`.
fn reload_clones(app: &ProxyApp) {
    let path = PathBuf::from(app.data_dir()).join("state.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(state) = serde_json::from_slice::<wire::ControlState>(&bytes) else {
        tracing::warn!(target: "groupproxy", "state.json did not parse; keeping the previous clone map");
        return;
    };
    let rows = state
        .hosts
        .into_iter()
        .map(|h| (h.id, CloneRow { group: h.group, archived: h.archived }))
        .collect();
    *app.clones.write().unwrap() = rows;
}

/// Poll the two files the sidecar reads but does not watch: `cliproxy-instances.json` (ports,
/// keys, admin secret) on [`SECRETS_POLL`] and `clone-tokens.json` (lifecycle epochs) on
/// [`EPOCH_POLL`]. Polling rather than watching because both are rewritten wholesale by the
/// control-server on ordinary activity, staleness costs at most a retryable per-request miss,
/// and a poll of two small files is cheaper to reason about than debouncing their renames.
async fn run_pollers(app: ProxyApp, admin_secret: Arc<RwLock<Option<String>>>) {
    let mut epoch_due = tokio::time::Instant::now();
    loop {
        app.cliproxy.reload();
        *admin_secret.write().unwrap() = app.cliproxy.admin_secret();
        if tokio::time::Instant::now() >= epoch_due {
            let data_dir = app.data_dir();
            let epochs =
                tokio::task::spawn_blocking(move || crate::tokens::read_lifecycle_epochs(&data_dir))
                    .await
                    .unwrap_or_default();
            *app.epochs.write().unwrap() = epochs;
            epoch_due = tokio::time::Instant::now() + EPOCH_POLL;
        }
        tokio::time::sleep(SECRETS_POLL).await;
    }
}

// --- entrypoint ---------------------------------------------------------------------------

/// The `group-proxy` subcommand entry (dispatched by argv in `main.rs`, mirroring
/// `self-upgrade`). Runs on the process's existing tokio runtime and never returns to normal
/// server boot: this process is the sidecar, not a control-server.
///
/// It brings up, in order: the shared-volume readers, the per-group CLIProxyAPI supervisor
/// ([`crate::cliproxy::run`]), the token-delta flusher, and the HTTP surface on [`PORT`].
pub async fn main() -> ! {
    let code = run().await;
    std::process::exit(code);
}

async fn run() -> i32 {
    // config.json lives in the shared /data volume (WORKDIR /data). READ-ONLY: `config::load`
    // can rewrite the file (legacy migration / default-group seeding) and the control-server
    // already did that at its own boot — this process is never a second writer.
    let cfg = match crate::config::load_read_only() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(target: "groupproxy", "loading config.json: {e:#}");
            return 1;
        }
    };
    let data_dir = cfg.data_dir.clone();
    tracing::info!(
        target: "groupproxy",
        "group-proxy starting: {} group(s), data_dir {data_dir}",
        cfg.groups.len()
    );

    // READ-ONLY on the secrets file: the control-server is its sole writer (see cliproxy.rs).
    let cliproxy = Arc::new(CliProxyManager::load_read_only(&data_dir));
    let admin_secret = Arc::new(RwLock::new(cliproxy.admin_secret()));
    let http = reqwest::Client::builder()
        .user_agent("rmng-group-proxy")
        .build()
        .expect("reqwest client");
    let control_url = format!(
        "http://{}:{}/internal/tokens",
        crate::docker::CONTROL_ALIAS,
        cfg.listen.web
    );
    let tokens = Arc::new(RemoteSink::new(control_url, admin_secret.clone(), http.clone()));

    let app = ProxyApp {
        cliproxy: cliproxy.clone(),
        cfg: Arc::new(RwLock::new(cfg)),
        clones: Arc::new(RwLock::new(HashMap::new())),
        epochs: Arc::new(RwLock::new(crate::tokens::read_lifecycle_epochs(&data_dir))),
        proxy_http: reqwest::Client::builder()
            .user_agent("rmng-group-proxy")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("transparent proxy client"),
        tokens: tokens.clone(),
    };

    spawn_input_watcher(app.clone());
    tokio::spawn(run_pollers(app.clone(), admin_secret));
    tokio::spawn(crate::cliproxy::run(SupervisorCtx {
        cliproxy,
        cfg: app.cfg.clone(),
    }));
    tokio::spawn(tokens.run_flusher());

    let addr = format!("0.0.0.0:{PORT}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(target: "groupproxy", "binding {addr}: {e}");
            return 1;
        }
    };
    tracing::info!(target: "groupproxy", "group-proxy router on http://{addr}/cc");
    if let Err(e) = axum::serve(listener, router(app)).await {
        tracing::error!(target: "groupproxy", "serving: {e}");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clone-facing URLs are baked into every clone's `/etc/environment` and generated
    /// agent configs, so a typo here silently strands the whole fleet's inference. Pin them.
    #[test]
    fn clone_facing_urls_point_at_the_sidecar() {
        assert_eq!(cc_base_url(), "http://rmng-cliproxy:9010/cc");
        assert_eq!(cc_v1_base_url(), "http://rmng-cliproxy:9010/cc/v1");
        // NOT the control-server: routing agents back through it is the regression this whole
        // feature exists to prevent.
        assert!(!cc_base_url().contains(crate::docker::CONTROL_ALIAS));
    }

    #[test]
    fn admin_urls_carry_group_and_path() {
        assert_eq!(
            admin_mgmt_url("team-a", "/anthropic-auth-url"),
            "http://rmng-cliproxy:9010/admin/team-a/mgmt/anthropic-auth-url"
        );
        assert_eq!(
            admin_mgmt_url("g", "/auth-files?name=claude-a%40b.json"),
            "http://rmng-cliproxy:9010/admin/g/mgmt/auth-files?name=claude-a%40b.json"
        );
        assert_eq!(
            admin_catalog_url("pool_1.beta"),
            "http://rmng-cliproxy:9010/admin/pool_1.beta/v1/models"
        );
        // A name outside the safe-group set can't inject extra path segments.
        assert_eq!(
            admin_catalog_url("a/b"),
            "http://rmng-cliproxy:9010/admin/a%2Fb/v1/models"
        );
    }

    /// The admin surface is what keeps the instances' management APIs out of clone reach, so
    /// its auth must fail CLOSED in every degenerate case — most importantly before the
    /// control-server has minted the secret at all.
    #[test]
    fn admin_auth_accepts_only_an_exact_minted_secret() {
        assert!(admin_authorized(Some("s3cret"), Some("s3cret")));
        assert!(!admin_authorized(Some("s3cret"), Some("wrong")));
        assert!(!admin_authorized(Some("s3cret"), None));
        // Not yet minted ⇒ deny, never fall open.
        assert!(!admin_authorized(None, Some("anything")));
        assert!(!admin_authorized(None, None));
        // A blank stored secret is not a wildcard.
        assert!(!admin_authorized(Some(""), Some("")));
    }

    fn sink() -> RemoteSink {
        RemoteSink::new(
            "http://unused/internal/tokens".into(),
            Arc::new(RwLock::new(None)),
            reqwest::Client::new(),
        )
    }

    /// The retry buffer must be BOUNDED: a control-server that stays down cannot be allowed to
    /// grow this process's memory without limit. New `(clone, epoch)` pairs are refused at the
    /// cap while existing ones keep accumulating.
    #[test]
    fn retry_buffer_is_bounded_but_keeps_coalescing() {
        let sink = sink();
        for i in 0..MAX_PENDING {
            assert!(
                sink.enqueue(TokenDelta {
                    host_id: format!("clone-{i}"),
                    epoch: 1,
                    input: 1,
                    ..Default::default()
                }),
                "pair {i} must fit under the cap"
            );
        }
        assert_eq!(sink.pending_len(), MAX_PENDING);
        // One more DISTINCT pair is refused.
        assert!(!sink.enqueue(TokenDelta {
            host_id: "overflow".into(),
            epoch: 1,
            input: 1,
            ..Default::default()
        }));
        assert_eq!(sink.pending_len(), MAX_PENDING, "the cap holds");
        // …but an already-tracked pair still coalesces, so an in-flight clone's accounting is
        // never silently frozen by another clone filling the buffer.
        assert!(sink.enqueue(TokenDelta {
            host_id: "clone-0".into(),
            epoch: 1,
            input: 41,
            output: 7,
            ..Default::default()
        }));
        assert_eq!(sink.pending_len(), MAX_PENDING);
        let batch = sink.take_batch();
        let merged = batch
            .iter()
            .find(|d| d.host_id == "clone-0")
            .expect("clone-0 present");
        assert_eq!((merged.input, merged.output), (42, 7));
        assert_eq!(sink.pending_len(), 0, "taking a batch drains the buffer");
    }

    /// The same clone at two different lifecycle epochs must NOT merge — an archive/unarchive
    /// between two responses is exactly what the epoch guard exists to separate.
    #[test]
    fn distinct_epochs_do_not_coalesce() {
        let sink = sink();
        sink.enqueue(TokenDelta { host_id: "h".into(), epoch: 1, input: 5, ..Default::default() });
        sink.enqueue(TokenDelta { host_id: "h".into(), epoch: 2, input: 9, ..Default::default() });
        assert_eq!(sink.pending_len(), 2);
        let mut inputs: Vec<u64> = sink.take_batch().iter().map(|d| d.input).collect();
        inputs.sort_unstable();
        assert_eq!(inputs, vec![5, 9]);
    }

    // --- the /cc router: token → clone → group → port resolution ---------------------

    /// A ProxyApp over a throwaway data dir, with the instances file opened as a WRITER so the
    /// test can allocate group meta and router keys the way the control-server does.
    fn test_proxy_app() -> (ProxyApp, Arc<CliProxyManager>, PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmng-groupproxy-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.to_string_lossy().into_owned();

        // The control-server's writer, and the sidecar's read-only view of the same file.
        let writer = Arc::new(CliProxyManager::load(&data_dir));
        let reader = Arc::new(CliProxyManager::load_read_only(&data_dir));
        let cfg = AppConfig { data_dir, ..Default::default() };
        let http = reqwest::Client::new();
        let app = ProxyApp {
            cliproxy: reader,
            cfg: Arc::new(RwLock::new(cfg)),
            clones: Arc::new(RwLock::new(HashMap::new())),
            epochs: Arc::new(RwLock::new(HashMap::new())),
            proxy_http: http.clone(),
            tokens: Arc::new(RemoteSink::new(
                "http://unused/internal/tokens".into(),
                Arc::new(RwLock::new(None)),
                http,
            )),
        };
        (app, writer, dir)
    }

    fn cc_request(auth: Option<&str>) -> Request {
        let mut b = axum::http::Request::builder()
            .method("POST")
            .uri("/cc/v1/messages");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn cc_proxy_missing_or_unknown_bearer_is_401() {
        let (app, _writer, dir) = test_proxy_app();
        assert_eq!(
            cc_proxy(State(app.clone()), cc_request(None)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            cc_proxy(State(app), cc_request(Some("Bearer nope"))).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cc_proxy_clone_without_group_is_409() {
        let (app, writer, dir) = test_proxy_app();
        let key = writer.mint_router_key("h1");
        app.cliproxy.reload();
        app.clones.write().unwrap().insert(
            "h1".into(),
            CloneRow { group: String::new(), archived: false },
        );
        let resp = cc_proxy(State(app), cc_request(Some(&format!("Bearer {key}")))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cc_proxy_group_without_instance_is_503() {
        let (app, writer, dir) = test_proxy_app();
        let key = writer.mint_router_key("h1");
        app.cliproxy.reload();
        app.clones.write().unwrap().insert(
            "h1".into(),
            CloneRow { group: "ghost".into(), archived: false }, // never provisioned → no port
        );
        let resp = cc_proxy(State(app), cc_request(Some(&format!("Bearer {key}")))).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cc_proxy_resolves_group_then_dials_instance() {
        let (app, writer, dir) = test_proxy_app();
        let key = writer.mint_router_key("h1");
        // The control-server side allocates the group's instance meta (a stable loopback port).
        app.cfg.write().unwrap().groups.push(wire::Group { name: "g".into() });
        writer.ensure_meta_for_test("g");
        // …and the sidecar picks it up on its next secrets poll.
        app.cliproxy.reload();
        app.clones.write().unwrap().insert(
            "h1".into(),
            CloneRow { group: "g".into(), archived: false },
        );
        // Resolution passes token→clone→group→port; the loopback instance isn't running in a
        // unit test, so the forward fails → 502. Proves the whole resolution chain wired up.
        let resp = cc_proxy(State(app), cc_request(Some(&format!("Bearer {key}")))).await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cc_proxy_archived_clone_is_409() {
        let (app, writer, dir) = test_proxy_app();
        let key = writer.mint_router_key("h1");
        app.cliproxy.reload();
        app.clones.write().unwrap().insert(
            "h1".into(),
            CloneRow { group: "g".into(), archived: true },
        );
        let resp = cc_proxy(State(app), cc_request(Some(&format!("Bearer {key}")))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The admin surface must be unreachable with a clone's per-clone router bearer — that
    /// separation is the whole reason it has its own secret.
    #[tokio::test]
    async fn admin_forward_rejects_a_clone_router_key() {
        let (app, writer, dir) = test_proxy_app();
        let key = writer.mint_router_key("h1");
        writer.ensure_admin_secret();
        app.cliproxy.reload();

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/admin/g/v1/models")
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .header(ADMIN_HEADER, key.clone()) // even presented as the admin key
            .body(Body::empty())
            .unwrap();
        let resp = admin_catalog(State(app), AxPath("g".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hop_by_hop_matches_framing_headers_only() {
        let hop = hop_by_hop_headers(&axum::http::HeaderMap::new());
        assert!(hop.contains(&header::HOST));
        assert!(hop.contains(&header::CONNECTION));
        assert!(hop.contains(&header::CONTENT_LENGTH));
        assert!(hop.contains(&header::TRANSFER_ENCODING));
        // Content-type + authorization are NOT framing headers (authorization is handled
        // separately by the router; content-type must survive for text/event-stream).
        assert!(!hop.contains(&header::CONTENT_TYPE));
        assert!(!hop.contains(&header::AUTHORIZATION));
    }

    /// A failed flush folds the batch back in rather than dropping it — that requeue is what
    /// carries accounting across a control-server upgrade.
    #[test]
    fn requeue_preserves_totals_across_a_failed_flush() {
        let sink = sink();
        sink.enqueue(TokenDelta { host_id: "h".into(), epoch: 1, input: 10, ..Default::default() });
        let batch = sink.take_batch();
        assert_eq!(sink.pending_len(), 0);
        // A delta observed while the flush was in flight, then the failed batch coming back.
        sink.enqueue(TokenDelta { host_id: "h".into(), epoch: 1, output: 3, ..Default::default() });
        sink.requeue(batch);
        let back = sink.take_batch();
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].input, back[0].output), (10, 3));
    }
}
