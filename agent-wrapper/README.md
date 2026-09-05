# agent-wrapper

A small HTTP wrapper around the [pi coding agent](https://www.npmjs.com/package/@earendil-works/pi-coding-agent), run inside each RDP container on **:4096**. The control-server drives a persistent per-host chat through it, and the agent controls the desktop through the clone-local `desktop` MCP.

It holds one long-lived pi `AgentSession`, created lazily on the first prompt and kept for the process lifetime.

## HTTP API

| Method + path | Purpose |
|---|---|
| `POST /prompt` | Body `{ text }`. Queues a user turn and returns `202 { ok }`; reply and progress arrive over `/events`. Returns `409` while a turn is running. |
| `GET /events` | SSE `{ busy }` snapshot, activity lines, then reply/error events. |
| `POST /abort` | Interrupts the in-flight turn while keeping the session alive. |
| `GET /health` | Returns `ok`. |

Every reply carries `solicited: true`. pi has no background bash and no task notifications, so the autonomous reply the Claude Agent SDK could produce never fires.

The session is in memory only: a CoW clone boots a fresh wrapper and starts a new conversation.

## Model and auth

The session runs on `gpt-5.6-luna` at `xhigh` reasoning effort on the Codex "Fast" speed tier (`service_tier: priority`). None of the three is configurable: the fleet runs one model, and the clone's pushed Codex token is what authorizes it.

Auth is file-based. The control-server signs in, refreshes, and pushes `~/.codex/auth.json` into the clone (control-server `codex.rs`). `src/auth.ts` reads that file on every request, so a rotated token lands without a restart. The pushed file carries an empty `refresh_token` on purpose, so the store reports a far-future expiry and pi never tries to refresh.

## MCP

pi ships no MCP support, so [`pi-mcp-adapter`](https://www.npmjs.com/package/pi-mcp-adapter) bridges the servers into pi tools. The wrapper reads the control-server's neutral descriptor at `~/.config/rmng/mcp.json` (the single source of truth, already headless-filtered) and maps it to the adapter's config.

The desktop server is promoted to direct tools (`desktop_screenshot`, `desktop_left_click`, …), which replaces the old `alwaysLoad` flag. Linear stays behind the proxy tool. On the very first session of a fresh clone the adapter has no tool-metadata cache yet, so that turn reaches both servers through the `mcp` proxy tool instead.

## Config (environment)

| Var | Default | Notes |
|---|---|---|
| `AGENT_PORT` | `4096` | listen port |
| `CODEX_AUTH_PATH` | `~/.codex/auth.json` | the credential the control-server pushes |
| `PI_CODING_AGENT_DIR` | `~/.pi/agent` | pi's config dir; holds the global `AGENTS.md` and the MCP tool cache |
| `LINEAR_API_KEY` | unset | Linear hosted MCP identity, injected from the selected preset; empty skips it |
| `AGENT_INSTRUCTIONS_PATH` | `~/.config/rmng/agent-instructions.md` | editable agent playbook injected by the control-server; present and non-empty overrides the baked-in default |

The wrapper does not report clone status directly: the control-server owns liveness through Docker, and reads activity off this wrapper's `/events` `busy`/`activity` frames.

## Run / deploy

```sh
bun install
bun run src/server.ts
```

The control-server deploys it as a user systemd unit in each clone, built by `bun build --compile` into a single binary.

Two things that binary needs, both handled in `src/server.ts`. pi resolves each provider's OAuth flow through a variable import specifier, which no bundler can follow, so `registerBunOAuthFlows()` registers the statically bundled flows instead. And a caller-supplied `ResourceLoader` starts empty, so `reload()` has to run before `createAgentSession` or no extension loads and the agent gets no MCP tools.
