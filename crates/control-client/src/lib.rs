//! Typed HTTP + SSE client for the control-server port-2 web API, shared by the
//! `rmng` fleet CLI and integration tests. Response shapes are the [`wire`] types
//! verbatim — this crate adds transport + error surfacing, never its own schema.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use wire::{
    AppConfigRedacted, BoardColumn, ContainerStats, ControlState, ExecRequest, ExecResult,
    LedgerRange, LedgerSearch, Operation, RmngClone,
};

/// A connected control-server client.
#[derive(Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

/// This container's hostname, which for a managed clone is its clone id.
///
/// Read from `/proc/sys/kernel/hostname` (the live UTS namespace) rather than from the
/// `HOSTNAME` env var or `/etc/hostname`. The env var is a bash-ism that fish never sets and
/// that a long-lived process can carry from a previous container, and both files can be baked
/// into a committed image. The proc entry belongs to the running namespace and nothing else.
///
/// `None` off Linux, where the CLI is not inside a clone anyway.
fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
}

/// What narrows a ledger search beside its pattern. Every field absent searches the whole corpus.
#[derive(Debug, Default, Clone)]
pub struct LedgerFilter<'a> {
    /// One clone id. Absent searches every clone the ledger knows, live or retired.
    pub clone: Option<&'a str>,
    /// Epoch milliseconds, both inclusive.
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// `Some(true)` keeps only subagent turns, `Some(false)` only the conversation.
    pub sidechain: Option<bool>,
    /// One subagent's id, as a hit's `agentId`.
    pub agent: Option<&'a str>,
    /// Hits to return. Absent takes the server's default of 50.
    pub limit: Option<usize>,
}

/// Optional ticket/preset/account overrides for a gen-2 fork. Every field is
/// `None` = inherit the source clone's binding.
#[derive(Debug, Clone, Default)]
pub struct ForkOpts<'a> {
    pub preset: Option<&'a str>,
    /// Ticket metadata override as a ready JSON object (camelCase keys:
    /// workspace, ticket, ticketUrl, branch, displayName, label).
    pub linear: Option<Value>,
    pub claude_account: Option<&'a str>,
    pub codex_account: Option<&'a str>,
    pub first_message: Option<&'a str>,
    pub agent_instructions: Option<&'a str>,
    pub claude_instructions: Option<&'a str>,
    pub headless: bool,
}

impl Client {
    /// `base` is the web-API origin, e.g. `http://rmng-control:9000` (no trailing slash).
    pub fn new(base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        Self {
            base,
            http: reqwest::Client::new(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Surface a non-2xx response as an error carrying the API's message. Handlers
    /// return either a plain-string body or `{ "error": … }` — accept both.
    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or(body);
        bail!("{status}: {}", msg.trim())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.http.get(format!("{}{path}", self.base)).send().await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Attach this process's own clone identity, when it has one.
    ///
    /// Both headers are a fallback. The server identifies a calling clone by the address the
    /// request arrives on, and reads these only when that address names no clone: the CLI run
    /// on an operator's box against a remote server, a proxy in front, dev mode.
    ///
    /// `RMNG_PROXY_KEY` is present in a clone's environment and absent on an operator laptop,
    /// so it decides whether the caller is inside the fleet at all, and nothing is sent without
    /// it. `X-RMNG-Clone` carries the container's hostname, which is the clone id, and decides
    /// *which* clone. The hostname outranks the key because the key can be stale: it reaches a
    /// process through `/etc/environment` → the lingering `systemd --user` manager → every
    /// session child, and a process keeps the environment it was launched with for life. A clone
    /// booted from an image that baked another clone's key ran its whole desktop session
    /// (terminals, editors, agents) under that clone's identity.
    fn with_identity(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let Some(key) = std::env::var("RMNG_PROXY_KEY")
            .ok()
            .filter(|k| !k.is_empty())
        else {
            return req;
        };
        let req = req.header("X-RMNG-Proxy-Key", key);
        match hostname().filter(|h| !h.is_empty()) {
            Some(host) => req.header("X-RMNG-Clone", host),
            None => req,
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Current fleet state, single-shot. `GET /api/state`; falls back to reading the
    /// first default `/events` frame against a server predating the endpoint. That
    /// fallback triggers on a non-JSON reply, not just 404 — an old server serves the
    /// frontend's index.html (200, text/html) for any unknown route via the SPA
    /// fallback.
    pub async fn state(&self) -> Result<ControlState> {
        let resp = self
            .http
            .get(format!("{}/api/state", self.base))
            .send()
            .await?;
        let json_ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.starts_with("application/json"))
            .unwrap_or(false);
        if resp.status() == reqwest::StatusCode::NOT_FOUND
            || (resp.status().is_success() && !json_ct)
        {
            let mut events = self.events().await?;
            return events
                .next()
                .await
                .ok_or_else(|| anyhow!("event stream ended before a state frame"))?;
        }
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Current volatile per-clone resource-usage map, matching the named `stats` SSE snapshot.
    pub async fn stats(&self) -> Result<HashMap<String, ContainerStats>> {
        self.get_json("/api/stats").await
    }

    /// The `/events` SSE stream, filtered to the default (unnamed) frames = full
    /// [`ControlState`] snapshots: one on connect, then one per change. Named events
    /// (`stats`, `forwards`) and keep-alive comments are skipped.
    pub async fn events(&self) -> Result<impl Stream<Item = Result<ControlState>> + Unpin> {
        let resp = self
            .http
            .get(format!("{}/events", self.base))
            .header("accept", "text/event-stream")
            .send()
            .await?;
        let resp = Self::check(resp).await?;
        let bytes = resp.bytes_stream();
        let stream = futures::stream::unfold(
            (
                bytes,
                SseParser::default(),
                std::collections::VecDeque::new(),
            ),
            |(mut bytes, mut parser, mut queue)| async move {
                loop {
                    if let Some(item) = queue.pop_front() {
                        return Some((item, (bytes, parser, queue)));
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            for ev in parser.push(&chunk) {
                                if ev.event.is_none() {
                                    queue.push_back(
                                        serde_json::from_str::<ControlState>(&ev.data)
                                            .map_err(|e| anyhow!("bad state frame: {e}")),
                                    );
                                }
                            }
                        }
                        Some(Err(e)) => return Some((Err(e.into()), (bytes, parser, queue))),
                        None => return None,
                    }
                }
            },
        );
        Ok(Box::pin(stream))
    }

    /// Select the clone shown in the viewer (`None` clears the selection).
    pub async fn activate(&self, id: Option<&str>) -> Result<ControlState> {
        self.post_json("/api/activate", &json!({ "id": id })).await
    }

    /// Start a template clone: `{ plain: { title, message } }` plus an optional preset
    /// name. The hostname derives server-side and the image builds on demand from the
    /// preset's Dockerfile.
    pub async fn clone_create_plain(
        &self,
        title: &str,
        message: &str,
        preset: Option<&str>,
    ) -> Result<Operation> {
        let mut body = json!({ "plain": { "title": title.trim(), "message": message.trim() } });
        if let Some(p) = preset.map(str::trim).filter(|p| !p.is_empty()) {
            body.as_object_mut()
                .unwrap()
                .insert("preset".into(), json!(p));
        }
        let req = self
            .http
            .post(format!("{}/api/clone", self.base))
            .json(&body);
        let v: Value = Self::check(req.send().await?).await?.json().await?;
        Ok(serde_json::from_value(
            v.get("op")
                .cloned()
                .ok_or_else(|| anyhow!("clone reply missing op"))?,
        )?)
    }

    /// Fork a gen-2 clone (snapshot + clone the source home).
    pub async fn fork(&self, source: &str, new_id: &str, headless: bool) -> Result<Operation> {
        self.post_json(
            "/api/fork",
            &json!({ "source": source, "hostname": new_id, "headless": headless }),
        )
        .await
    }

    /// Fork with optional ticket/preset/account overrides (`None` = inherit the source).
    /// Keys are camelCase to match the server's `ForkReq`.
    pub async fn fork_with(
        &self,
        source: &str,
        new_id: &str,
        opts: &ForkOpts<'_>,
    ) -> Result<Operation> {
        let mut body = json!({ "source": source, "hostname": new_id });
        let obj = body.as_object_mut().unwrap();
        if opts.headless {
            obj.insert("headless".into(), json!(true));
        }
        for (k, v) in [
            ("preset", opts.preset),
            ("claudeAccount", opts.claude_account),
            ("codexAccount", opts.codex_account),
            ("firstMessage", opts.first_message),
            ("agentInstructions", opts.agent_instructions),
            ("claudeInstructions", opts.claude_instructions),
        ] {
            if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
                obj.insert(k.into(), json!(v));
            }
        }
        if let Some(linear) = &opts.linear {
            obj.insert("linear".into(), linear.clone());
        }
        self.post_json("/api/fork", &body).await
    }

    /// Rebase a gen-2 clone onto a new base tag (dataset + id kept).
    pub async fn rebase(&self, id: &str, tag: &str) -> Result<Operation> {
        self.post_json(&format!("/api/hosts/{id}/rebase"), &json!({ "tag": tag }))
            .await
    }

    /// Destroy a managed clone (or unregister a plain clone).
    pub async fn delete(&self, id: &str) -> Result<Operation> {
        self.post_json("/api/delete", &json!({ "id": id })).await
    }

    /// Stop a managed clone while retaining its container, volumes, notes, and chat history.
    pub async fn archive(&self, id: &str) -> Result<Operation> {
        self.post_json(&format!("/api/hosts/{id}/archive"), &json!({}))
            .await
    }

    /// Restart a retained archived clone.
    pub async fn unarchive(&self, id: &str) -> Result<Operation> {
        self.post_json(&format!("/api/hosts/{id}/unarchive"), &json!({}))
            .await
    }

    /// Hot-swap a clone's Claude account. `account` is a selection: an email, `auto`,
    /// `none`, or `group:<pool>`. `POST /api/claude/swap`.
    pub async fn claude_swap(&self, host: &str, account: &str) -> Result<Value> {
        self.post_json(
            "/api/claude/swap",
            &json!({ "host": host, "account": account }),
        )
        .await
    }

    /// Hot-swap a clone's Codex account. `POST /api/codex/swap`.
    pub async fn codex_swap(&self, host: &str, account: &str) -> Result<Value> {
        self.post_json(
            "/api/codex/swap",
            &json!({ "host": host, "account": account }),
        )
        .await
    }

    /// Delete an imported Claude account by email. Errors (surfaced by `check`) if a clone
    /// is pinned to it. Returns the API's `{ ok, moved: [clone ids] }`.
    pub async fn claude_delete(&self, account: &str) -> Result<Value> {
        self.post_json("/api/claude/delete", &json!({ "account": account }))
            .await
    }

    /// Delete an imported Codex account by email.
    pub async fn codex_delete(&self, account: &str) -> Result<Value> {
        self.post_json("/api/codex/delete", &json!({ "account": account }))
            .await
    }

    /// The redacted server config (presets, account groups, docker settings — no secrets).
    pub async fn config(&self) -> Result<AppConfigRedacted> {
        self.get_json("/api/config").await
    }

    /// Proxy a desktop-automation tool call to a clone's daemon MCP
    /// (`POST /api/hosts/:id/mcp`). Returns the daemon's `content` array verbatim (a
    /// JSON array of `{type:"text",…}` / `{type:"image",…}` items). Unknown clone → the
    /// server's 404, daemon error → its 502, both surfaced as errors by `check`.
    pub async fn desktop(&self, host: &str, tool: &str, args: Value) -> Result<Value> {
        self.post_json(
            &format!("/api/hosts/{host}/mcp"),
            &json!({ "tool": tool, "args": args }),
        )
        .await
    }

    /// Search every clone's distilled transcripts (`GET /api/ledger/search`), retired clones
    /// included. `pattern` is a case-insensitive substring of the whole ledger line. The search
    /// runs server-side, so what comes back is the matching lines rather than the corpus.
    pub async fn ledger_search(
        &self,
        pattern: &str,
        filter: &LedgerFilter<'_>,
    ) -> Result<LedgerSearch> {
        let mut query: Vec<(&str, String)> = vec![("q", pattern.to_string())];
        if let Some(id) = filter.clone {
            query.push(("clone", id.to_string()));
        }
        if let Some(ms) = filter.since {
            query.push(("since", ms.to_string()));
        }
        if let Some(ms) = filter.until {
            query.push(("until", ms.to_string()));
        }
        if let Some(want) = filter.sidechain {
            query.push(("sidechain", want.to_string()));
        }
        if let Some(id) = filter.agent {
            query.push(("agent", id.to_string()));
        }
        if let Some(n) = filter.limit {
            query.push(("limit", n.to_string()));
        }
        let resp = self
            .http
            .get(format!("{}/api/ledger/search", self.base))
            .query(&query)
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// A byte range of one session's ledger (`GET /api/ledger/read`), snapped outward to whole
    /// NDJSON lines. Pass an offset below a hit's own to read what led up to it.
    pub async fn ledger_read(
        &self,
        clone: &str,
        session: &str,
        offset: u64,
        len: u64,
    ) -> Result<LedgerRange> {
        let resp = self
            .http
            .get(format!("{}/api/ledger/read", self.base))
            .query(&[
                ("clone", clone.to_string()),
                ("session", session.to_string()),
                ("offset", offset.to_string()),
                ("len", len.to_string()),
            ])
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Run a single non-interactive command inside a clone (`POST /api/hosts/:id/exec`).
    /// Returns the command's exit code plus its captured stdout/stderr.
    pub async fn exec(&self, host: &str, req: &ExecRequest) -> Result<ExecResult> {
        self.post_json(
            &format!("/api/hosts/{host}/exec"),
            &serde_json::to_value(req)?,
        )
        .await
    }

    /// Replace the board's columns wholesale.
    ///
    /// The server applies no rules here, so the caller sends the settled list. `wire::board`
    /// holds those rules, mirrored from the browser's copy so both clients arrange a board
    /// the same way. `PUT`, matching the route and the browser.
    pub async fn board_put(&self, columns: &[BoardColumn]) -> Result<ControlState> {
        let resp = self
            .http
            .put(format!("{}/api/board", self.base))
            .json(&json!({ "columns": columns }))
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// This process's own clone record, or `None` when not running inside a clone.
    pub async fn clone_self(&self) -> Result<Option<RmngClone>> {
        let req = Self::with_identity(self.http.get(format!("{}/api/self", self.base)));
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Self::check(resp).await?.json().await?)
    }
}

/// True when `err` is a transport failure — the server was unreachable (connection
/// refused) or timed out — rather than an error the server itself returned (a 4xx/5xx
/// surfaced by [`Client::check`], which is a plain string error with no `reqwest::Error`
/// in its chain). The CLI uses this to decide whether the "check your --server" hint is
/// actually relevant: a `404 no clone 'x'` from a perfectly reachable server should not
/// nudge the caller toward a connectivity fix.
pub fn is_transport_error(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        e.downcast_ref::<reqwest::Error>()
            .is_some_and(|re| re.is_connect() || re.is_timeout())
    })
}

/// One parsed SSE event: optional `event:` name + the joined `data:` payload.
#[derive(Debug, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental server-sent-events parser. Feed raw chunks; complete events come out.
/// Handles the subset the control-server emits: `event:`/`data:` fields, comment
/// keep-alives (`: ping`), events terminated by a blank line. Buffers bytes so a
/// UTF-8 code point or line split across chunks reassembles correctly (`\n` is
/// single-byte in UTF-8, so splitting on it never lands inside a code point).
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        // Consume complete lines; keep the trailing partial line buffered.
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                // Blank line = event boundary.
                if !self.data.is_empty() {
                    out.push(SseEvent {
                        event: self.event.take(),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                } else {
                    self.event = None;
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data
                    .push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("event:") {
                self.event = Some(rest.trim().to_string());
            }
            // Comments (leading ':') and unknown fields are ignored per the SSE spec.
        }
        out
    }
}

#[cfg(test)]
mod sse_tests {
    use super::*;

    fn parse_all(chunks: &[&str]) -> Vec<SseEvent> {
        let mut p = SseParser::default();
        chunks.iter().flat_map(|c| p.push(c.as_bytes())).collect()
    }

    #[test]
    fn api_error_is_not_a_transport_error() {
        // A server-returned error (the shape `check` produces) must not be treated as a
        // connectivity failure — otherwise the CLI would wrongly print the --server hint.
        let err = anyhow::anyhow!("404 Not Found: no clone 'x'");
        assert!(!is_transport_error(&err));
    }

    #[test]
    fn parses_default_event() {
        let evs = parse_all(&["data: {\"hosts\":[]}\n\n"]);
        assert_eq!(
            evs,
            vec![SseEvent {
                event: None,
                data: "{\"hosts\":[]}".into()
            }]
        );
    }

    #[test]
    fn parses_named_event_and_keeps_name() {
        let evs = parse_all(&["event: stats\ndata: {}\n\n"]);
        assert_eq!(
            evs,
            vec![SseEvent {
                event: Some("stats".into()),
                data: "{}".into()
            }]
        );
    }

    #[test]
    fn skips_keepalive_comments() {
        let evs = parse_all(&[": ping\n\n", "data: 1\n\n"]);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "1");
    }

    #[test]
    fn reassembles_events_split_across_chunks() {
        let evs = parse_all(&["data: {\"sel", "ected\":null}", "\n", "\n"]);
        assert_eq!(
            evs,
            vec![SseEvent {
                event: None,
                data: "{\"selected\":null}".into()
            }]
        );
    }

    #[test]
    fn name_does_not_leak_into_next_event() {
        let evs = parse_all(&["event: stats\ndata: a\n\ndata: b\n\n"]);
        assert_eq!(evs[0].event.as_deref(), Some("stats"));
        assert_eq!(evs[1].event, None);
    }

    #[test]
    fn handles_crlf_lines() {
        let evs = parse_all(&["data: x\r\n\r\n"]);
        assert_eq!(evs[0].data, "x");
    }

    #[test]
    fn multibyte_utf8_split_across_chunks_survives() {
        let s = "data: {\"note\":\"éclair\"}\n\n".as_bytes();
        let (a, b) = s.split_at(13); // splits inside the two-byte 'é'
        let mut p = SseParser::default();
        let mut evs = p.push(a);
        evs.extend(p.push(b));
        assert_eq!(evs[0].data, "{\"note\":\"éclair\"}");
    }
}
