# agent-wrapper

A small HTTP wrapper around the [Claude Agent SDK](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk), run inside each RDP container on **:4096**. The control-server drives a persistent per-host chat through it, and the agent controls the desktop through the clone-local `desktop` MCP.

It holds one long-lived streaming-input session created lazily on the first prompt. Background task notifications re-engage the session automatically, so a detached command can complete without requiring a second dashboard prompt.

## HTTP API

| Method + path | Purpose |
|---|---|
| `POST /prompt` | Body `{ text }`. Queues a user turn and returns `202 { ok }`; reply and progress arrive over `/events`. Returns `409` while a turn is running. |
| `GET /events` | SSE `{ busy }` snapshot, activity lines, then reply/error events. |
| `POST /abort` | Interrupts the in-flight turn while keeping the session alive. |
| `GET /health` | Returns `ok`. |

The session id is in memory only: a CoW clone boots a fresh wrapper and starts a new conversation.

## Config (environment)

| Var | Default | Notes |
|---|---|---|
| `AGENT_PORT` | `4096` | listen port |
| `AGENT_MODEL` | `claude-opus-4-8` | Claude model id |
| `AGENT_EXECUTABLE` | `node` | JavaScript runtime for the bundled CLI |
| `LINEAR_API_KEY` | unset | Linear hosted MCP identity, injected from the selected preset; empty skips it |
| `AGENT_INSTRUCTIONS_PATH` | `~/.config/rmng/agent-instructions.md` | editable agent playbook injected by the control-server; present and non-empty overrides the baked-in default |

The wrapper always registers the clone-local desktop MCP (`http://127.0.0.1:9004`). It optionally registers the hosted Linear MCP. It does not report clone status: the control server owns liveness and activity through Docker and passive CLIProxy traffic.

## Run / deploy

```sh
bun install
bun run src/server.ts
```

The control-server deploys it as a user systemd unit in each clone.
