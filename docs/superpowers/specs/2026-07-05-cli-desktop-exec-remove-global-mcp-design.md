# Move fleet desktop control to the CLI; remove the global MCP; add `rmng exec`

**Date:** 2026-07-05
**Status:** Approved — implementation

## Summary

Retire the control-server's **global MCP** (`:9003`), the operator/fleet-facing desktop
proxy. Fold its 14 desktop/window tools into the `rmng` CLI as a `desktop` subgroup, and
add a docker-exec-style `rmng exec` for running a single non-interactive command inside a
clone. The CLI's only dependency stays the port-2 web API; the control-server grows two
web endpoints that reach clones on the CLI's behalf.

**Unchanged:** the per-clone MCP (`:9002`, `set_state`) and each clone's daemon MCP
(`:9004`, the real desktop-automation surface). The new `/api/hosts/:id/mcp` endpoint
proxies to `:9004` verbatim — the daemon MCP stays the single source of truth for the
desktop tool schema.

## Motivation

Fleet **management** already lives in the `rmng` CLI. The global MCP is the last
orchestration-shaped surface that isn't in the CLI; its callers are external operators /
fleet agents (there are no in-repo consumers). Fleet agents are Claude Code instances that
drive clones through Bash, so a CLI is a more natural fit than a separate MCP server, and
removing `:9003` drops a running service and a published port.

## Architecture

```
rmng CLI ──HTTP──▶ control-server (port-2 web API) ──┬─▶ docker exec (bollard)  → clone container   [exec]
                                                     └─▶ POST http://{clone}:9004 (daemon MCP)       [desktop]
```

- The CLI never dials clones directly (it runs from operator machines outside the Docker
  network; clone IPs are dynamic Docker IPAM). Only the control-server resolves/reaches
  clones — via Docker DNS (`app.dial_host`) for the daemon MCP, and the bollard Docker
  socket for exec.
- **Chosen approach: generic daemon-MCP proxy endpoint + thin typed CLI** (vs. one typed
  REST endpoint per tool). Minimal server code, no schema duplication: the existing
  `proxy_to_daemon` fn moves from `mcp.rs` to `web.rs` and becomes the `/mcp` handler.

## Server changes (`crates/control-server`, `crates/wire`)

### New web endpoints (added in `web.rs` router, under the existing `/api/hosts/:id/…`)

1. `POST /api/hosts/:id/mcp` — body `{ "tool": string, "args": object }`.
   - Resolves host `:id` from state → `404` if unknown.
   - Wraps into JSON-RPC `tools/call` and forwards verbatim to `http://{id}:{daemon_mcp}/`
     via the moved `proxy_to_daemon` (uses `app.dial_host` + `app.http`).
   - Returns the daemon's `result.content` array as JSON. Daemon unreachable / JSON-RPC
     error → `502` with the daemon's message.
   - `proxy_to_daemon` **moves out of `mcp.rs` into `web.rs`** (or a shared spot both can
     reach); its behavior is unchanged.

2. `POST /api/hosts/:id/exec` — body = `wire::ExecRequest`, returns `wire::ExecResult`.
   - Unknown clone → `404`; container not running → `502`/clear error.
   - Runs via the **existing bollard exec primitive** in `docker.rs`
     (`CreateExecOptions` + `StartExecResults`, reusing `LineSplitter`). No TTY.
   - Default user **uid `1000`** (the clone's agent user — the `rmng` account that
     `rmng ssh` lands as); overridable. Honors `workdir`, `env`, and optional stdin.
   - Captures stdout and stderr **separately**, buffered; returns the command's exit code.

### New `wire` types (`crates/wire`, match existing camelCase serde conventions)

```rust
pub struct McpCallRequest { pub tool: String, pub args: serde_json::Value }

pub struct ExecRequest {
    pub cmd: Vec<String>,           // argv; empty → 400
    pub user: Option<String>,       // default uid "1000" applied server-side if None
    pub workdir: Option<String>,
    pub env: Vec<String>,           // "KEY=VAL" entries
    pub stdin_b64: Option<String>,  // base64 stdin, if any
}

pub struct ExecResult { pub exit_code: i64, pub stdout: String, pub stderr: String }
```
`stdout`/`stderr` are UTF-8-lossy strings (binary output is out of scope; add `--raw`
later if ever needed).

### Removals

- `main.rs` — delete the `tokio::spawn(mcp::serve(app.clone(), cfg.listen.global_mcp,
  mcp::Scope::Global))` line (~:205).
- `mcp.rs` — delete everything Global: `Scope::Global`, `dtool`, `is_daemon_tool`, the
  `tools_for(Global)` branch, and the Global-scope tests. `proxy_to_daemon` moves to
  `web.rs`. The `Scope` enum collapses to per-clone only; `serve()` drops its `scope`
  param. Result: `mcp.rs` is a lean per-clone `set_state`-only server.
- Config: remove `listen.global_mcp` from `crates/wire/src/config.rs` (field + default +
  the JSON config test that names `globalMcp`), and the `old.listen.global_mcp !=
  new.listen.global_mcp` check + the `n.listen.global_mcp = 8083;` test line in
  `control-server/src/config.rs`. **No config migration needed** — config structs do not
  use `deny_unknown_fields`, so already-persisted configs carrying `globalMcp` still
  deserialize (the field is ignored).

## Client changes (`crates/control-client`)

Add two `Client` methods mirroring the existing ones:

```rust
pub async fn desktop(&self, host: &str, tool: &str, args: Value) -> Result<Value>; // POST /api/hosts/:id/mcp → content array
pub async fn exec(&self, host: &str, req: &ExecRequest) -> Result<ExecResult>;      // POST /api/hosts/:id/exec
```

## CLI changes (`crates/cli`)

### `desktop` subgroup (alias `dt`) — `args.rs` + `commands.rs`

Clone is the first positional. Verbs map 1:1 to the 14 daemon tools:

```
rmng desktop <clone> screenshot [--monitor N] [--out PATH]
rmng desktop <clone> monitors | windows | apps
rmng desktop <clone> move X Y            [--monitor N] [--out PATH]
rmng desktop <clone> click [X Y]         [--monitor N] [--out PATH]
rmng desktop <clone> rclick [X Y] | mclick [X Y] | dclick [X Y]
rmng desktop <clone> scroll AMOUNT [X Y] [--monitor N] [--out PATH]
rmng desktop <clone> key "ctrl+c"        [--out PATH]
rmng desktop <clone> type "some text"    [--out PATH]
rmng desktop <clone> launch firefox.desktop
rmng desktop <clone> movewin <win-id> [--monitor N] [--mode maximize|center-half]
```

Verb → daemon tool name mapping: `move`→`mouse_move`, `click`→`left_click`,
`rclick`→`right_click`, `mclick`→`middle_click`, `dclick`→`left_double_click`,
`key`→`key`, `type`→`type`, `scroll`→`scroll`, `screenshot`→`screenshot`,
`monitors`→`list_monitors`, `windows`→`list_windows`, `apps`→`list_apps`,
`launch`→`launch_app`, `movewin`→`move_window`.

**Screenshot-on-every-action (the core UX rule):**
- **Action verbs** (`move`, `click`, `rclick`, `mclick`, `dclick`, `scroll`, `key`,
  `type`, `launch`, `movewin`) always produce a post-action JPEG: write it to a file and
  print the **absolute path** on stdout for the agent to `Read`.
  - Most daemon action tools already return the settle-screenshot as `image` content —
    decode that base64 and write it.
  - For tools whose result has no image (`type` → `"typed N chars"`; `launch`/`movewin`
    → JSON), the CLI issues a **follow-up `desktop … screenshot`** call (monitor 0 or
    `--monitor N`) so the guarantee holds uniformly. Any text/JSON result is printed too,
    before the path.
- **Query verbs** (`monitors`, `windows`, `apps`) print their JSON result; no screenshot.
- `screenshot` writes the JPEG and prints the path.
- Default file path when `--out` omitted: `$TMPDIR/rmng-<clone>-mon<N>.jpg`
  (`std::env::temp_dir()`), overwritten each call; `--out PATH` overrides.

### `exec` subcommand — `args.rs` + `commands.rs`

```
rmng exec <clone> [-u|--user USER] [-w|--workdir DIR] [-e|--env KEY=VAL ...] -- <cmd> [args...]
```
- Non-interactive, no TTY. Terminal stdin is read and passed through (base64 in
  `ExecRequest.stdin_b64`) so `echo hi | rmng exec c -- cat` works.
- Command stdout → CLI stdout, stderr → CLI stderr (kept separate). CLI **exits with the
  command's exit code**.
- Global `--json` flag: emit `{exit_code, stdout, stderr}` as one object instead of
  splitting streams.
- Repeated `-e` accumulates; `--` separates rmng flags from the command argv.

## Docs & ancillary

- `docs/MCP.md` — drop the global-MCP section + curl examples; retitle to the two
  surviving surfaces (per-clone `:9002`, daemon `:9004`); note desktop control now lives
  in `rmng desktop` (link CLI.md).
- `docs/CLI.md` — add `desktop` and `exec` sections (verb table + screenshot rule + exec
  flags/exit-code semantics).
- Port range `9000-9003` → `9000-9002` in: `README.md`, `compose.yaml`,
  `crates/control-server/README.md`, `docs/DEPLOY.md`, `scripts/e2e-agent-playbook.sh`.
  (`:9004` daemon MCP and `:9005` forward are untouched.)
- `frontend/app/stories/fixtures.ts` — remove `globalMcp` from the `listen` fixture.
- **Migration note (docs only, no code):** external configs registering `:9003` as an MCP
  server must switch to `rmng desktop`. No in-repo consumers.

## Testing

- `mcp.rs` — reduce tests to the per-clone/`set_state` assertion; delete Global-scope
  tests.
- `web.rs` — `/api/hosts/:id/mcp` unknown clone → 404; `/api/hosts/:id/exec` exit-code +
  stdout/stderr passthrough (against a running clone in the e2e harness, or a unit test of
  the request/response mapping).
- `cli/args.rs` — parse tests: `desktop` subgroup verbs + `exec` (the `--` separator,
  repeated `-e`, `-u`/`-w`).
- `scripts/e2e-agent-playbook.sh` — updated port range; optional smoke:
  `rmng exec <clone> -- echo hi` (exit 0, stdout `hi`) and
  `rmng desktop <clone> screenshot` (prints a readable path).

## Non-goals / YAGNI

- No streaming exec, no TTY, no interactive stdin loop (`rmng ssh` covers that).
- No binary/raw exec output (UTF-8-lossy only).
- No typed REST endpoint per desktop tool (generic `/mcp` proxy instead).
- Per-clone `:9002` and daemon `:9004` MCPs are untouched.
