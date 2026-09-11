# control-client

The typed HTTP + SSE client for the control-server's **port-2 web API**, shared by the
[`rmng` fleet CLI](../cli/README.md) and integration tests. Response shapes are the
[`wire`](../wire/README.md) types verbatim — this crate adds transport + error surfacing,
never its own schema.

## What it offers

- `Client::state()` — single-shot `ControlState` via `GET /api/state`, falling back to the
  first default `/events` frame against a server that predates the endpoint (the fallback
  triggers on any non-JSON reply, not just 404 — an old server answers unknown routes with
  the SPA's `index.html`).
- `Client::events()` — `/events` as a typed `Stream<Item = Result<ControlState>>`: default
  (unnamed) frames only; named `stats`/`forwards` events and keep-alive comments are skipped.
- Typed wrappers for the fleet actions: `activate`, `start_clone` (`POST /api/clone` and
  `POST /api/fork`, both taking a `wire::CloneRequest`), `delete`, `images`/`image_pull`/`image_commit`/`image_delete`,
  `claude_swap`/`codex_swap`, `config` (redacted).
- `SseParser` — incremental SSE frame parser (`event:`/`data:` fields, `:` comment
  keep-alives, blank-line terminators, chunk-split reassembly incl. multi-byte UTF-8).

Errors: a non-2xx reply surfaces as an `anyhow` error carrying the API's message (handlers
return either a plain-string body or `{error}` — both are accepted).

## Dependencies

`reqwest` (async), `wire`, `futures`, `tokio`, `serde`/`serde_json`, `anyhow`. No CLI here —
the binary lives in [`crates/cli`](../cli/README.md).

## Tests

`cargo test -p control-client`: the SSE parser (named vs. default events, comment
keep-alives, CRLF, frames + multi-byte UTF-8 split across chunks).
