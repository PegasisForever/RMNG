//! Typed HTTP + SSE client for the control-server port-2 web API, shared by the
//! `rmng` fleet CLI and integration tests. Response shapes are the [`wire`] types
//! verbatim — this crate adds transport + error surfacing, never its own schema.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use wire::{
    AppConfigRedacted, CloneTokenUsage, ContainerStats, ControlState, ExecRequest, ExecResult,
    ImageInfo, Operation,
};

/// A connected control-server client.
#[derive(Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
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

    /// Current safe per-clone cumulative-token map, matching the named `tokens` SSE snapshot.
    pub async fn tokens(&self) -> Result<HashMap<String, CloneTokenUsage>> {
        self.get_json("/api/tokens").await
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

    /// Select the host shown in the viewer (`None` clears the selection).
    pub async fn activate(&self, id: Option<&str>) -> Result<ControlState> {
        self.post_json("/api/activate", &json!({ "id": id })).await
    }

    /// Raw hostname clone (`POST /api/clone` hostname mode) with an optional account-group
    /// binding and env preset. `none`/blank group input intentionally clears the binding.
    pub async fn clone_host(
        &self,
        image: &str,
        hostname: &str,
        group: Option<&str>,
        preset: Option<&str>,
    ) -> Result<Operation> {
        let mut body = json!({ "image": image, "hostname": hostname });
        let obj = body.as_object_mut().unwrap();
        if let Some(group) = group
            .map(str::trim)
            .filter(|group| !group.is_empty() && *group != "none")
        {
            obj.insert("group".into(), json!(group));
        }
        if let Some(p) = preset {
            obj.insert("preset".into(), json!(p));
        }
        let v: Value = self.post_json("/api/clone", &body).await?;
        Ok(serde_json::from_value(
            v.get("op")
                .cloned()
                .ok_or_else(|| anyhow!("clone reply missing op"))?,
        )?)
    }

    /// Destroy a managed clone (or unregister a plain host).
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

    /// The clone-source images.
    pub async fn images(&self) -> Result<Vec<ImageInfo>> {
        self.get_json("/api/images").await
    }

    /// Pull the clone template (`None` = the configured default reference).
    pub async fn image_pull(&self, reference: Option<&str>) -> Result<Operation> {
        self.post_json("/api/images/pull", &json!({ "reference": reference }))
            .await
    }

    /// Commit a running clone to a new clone-source image `<name>:latest`.
    pub async fn image_commit(&self, host: &str, name: &str) -> Result<Operation> {
        self.post_json("/api/images/commit", &json!({ "host": host, "name": name }))
            .await
    }

    /// Remove a clone-source image (409 when in use).
    pub async fn image_delete(&self, reference: &str) -> Result<()> {
        let _: Value = self
            .post_json("/api/images/delete", &json!({ "reference": reference }))
            .await?;
        Ok(())
    }

    /// Bind a clone to an account group (or clear it with `None`). The group-proxy
    /// replacement for the old per-provider account swap. `POST /api/hosts/:id/group`.
    pub async fn set_host_group(&self, host: &str, group: Option<&str>) -> Result<Value> {
        self.post_json(
            &format!("/api/hosts/{host}/group"),
            &json!({ "group": group }),
        )
        .await
    }

    /// Remove one authenticated account (a credential file) from a group's CLIProxyAPI
    /// instance. `POST /api/groups/:name/accounts/delete`.
    pub async fn delete_group_account(&self, group: &str, file: &str) -> Result<Value> {
        self.post_json(
            &format!("/api/groups/{group}/accounts/delete"),
            &json!({ "file": file }),
        )
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

    /// Run a single non-interactive command inside a clone (`POST /api/hosts/:id/exec`).
    /// Returns the command's exit code plus its captured stdout/stderr.
    pub async fn exec(&self, host: &str, req: &ExecRequest) -> Result<ExecResult> {
        self.post_json(
            &format!("/api/hosts/{host}/exec"),
            &serde_json::to_value(req)?,
        )
        .await
    }
}

/// True when `err` is a transport failure — the server was unreachable (connection
/// refused) or timed out — rather than an error the server itself returned (a 4xx/5xx
/// surfaced by [`Client::check`], which is a plain string error with no `reqwest::Error`
/// in its chain). The CLI uses this to decide whether the "check your --server" hint is
/// actually relevant: a `404 no host 'x'` from a perfectly reachable server should not
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
        let err = anyhow::anyhow!("404 Not Found: no host 'x'");
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
