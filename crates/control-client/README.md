# control-client

A small reusable Rust client for the control-server's port-2 HTTP/SSE API: a reqwest-based
client plus an SSE frame reader, typed against [`wire`](../wire/README.md). Used by
**integration tests** (and any future Rust consumer of `/events`).

> The operator **CLI** that this crate used to back is **replaced by the global MCP
> (port 4)** — an operator agent or scripts drive everything through MCP tools instead of a
> bespoke CLI (`control-server-ctl` is retired at cutover). This crate stays as a thin,
> optional test client; it is **not** on any hot path and can land late.

## What it offers

- `Client` with `get_json`/`post_json`/`open_events()` over plain HTTP (**no TLS feature**,
  to keep one crypto provider process-wide).
- A typed SSE stream of `ControlState` from `/events`.
- The SSE frame parser worth reusing (the clean pieces from the old `control-server-ctl`):
  `data:`-line accumulation, blank-line frame terminators, `:`-comment/heartbeat handling.

## Dependencies

`reqwest` (async), `wire`, `serde_json`, `anyhow`. (No `clap` — no CLI binary here anymore.)

## Tests

- SSE frame parser unit tests (split frames, comments, heartbeats).
- Against a running control-server: `/events` decodes to `ControlState`; a `POST
  /api/activate` round-trips.
