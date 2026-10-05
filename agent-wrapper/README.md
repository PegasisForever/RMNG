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

Every reply carries `solicited: true`. pi has no background bash and no task notifications, so it never produces an autonomous (`solicited: false`) reply.

The session is in memory only: a CoW clone boots a fresh wrapper and starts a new conversation.

## Model and auth

The session runs on `gpt-6-luna` at `max` reasoning effort, on the default speed tier: no `service_tier` is sent, so the Codex "Fast" tier and its higher usage are never used. Neither is configurable: the fleet runs one model, and the clone's pushed Codex token is what authorizes it.

Auth is file-based. The control-server signs in, refreshes, and pushes `~/.codex/auth.json` into the clone (control-server `codex.rs`). `src/auth.ts` reads that file on every request, so a rotated token lands without a restart. The pushed file carries an empty `refresh_token` on purpose, so the store reports a far-future expiry and pi never tries to refresh.

## MCP

pi (1.0 and later) has MCP support built in, so no adapter extension is needed. An SDK session does not load it on its own, so the wrapper adds pi's MCP extension (`createMcpExtension`) and its `tool_search` extension (`createToolSearchExtension`) to the resource loader, then calls `bindExtensions`, which is when the servers connect. The wrapper reads the control-server's neutral descriptor at `~/.config/rmng/mcp.json` (the single source of truth, already headless-filtered) and hands those servers to the MCP extension in place of `~/.pi/agent/mcp.json`, so a `pi mcp add` inside the clone does not change what the assistant can reach.

pi names MCP tools `mcp__<server>__<tool>`. The desktop server is marked `directTools` in the descriptor, so it gets `direct` exposure and its tools (`mcp__desktop__screenshot`, `mcp__desktop__left_click`, …) are declared to the model like built-in tools; the first prompt waits for it to connect. Every other server, Linear included, gets `deferred` exposure: its tools are found and loaded with `tool_search`. Codemode is not used.

The loader runs with `noExtensions`, so the wrapper loads only its inline extensions (MCP, tool search, and the request log) and ignores anything under `~/.pi/agent/extensions`. A discovered extension would load ahead of them and could block a tool call or rewrite the provider payload before they run, so a `pi install` inside the clone must not reach the assistant.

The startup line the wrapper logs is a snapshot taken while the servers are still connecting, so it may not list the `mcp__desktop__*` tools yet. The first provider request also logs one line with the model, the reasoning effort, and the speed tier that actually went out.

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
