# agent-wrapper

A small HTTP wrapper around the [Claude Agent SDK](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk),
run inside each RDP container (one process per host) on **:4096** — the slot
`opencode serve` used to occupy. The control-server drives a single persistent
per-host chat through it, and the agent controls the desktop via the
`computer-use` MCP (registered here as **`desktop`**, since `computer-use` is a
reserved MCP name in Claude Code).

It replaces the opencode backend: same external contract (an HTTP service on
:4096 that the control-server talks to), different engine.

It holds **one long-lived streaming-input session** (created lazily on the first
prompt, kept alive for the process lifetime). That's what makes the **monitoring
loop** work: the agent starts the stuck-detector as a background command, ends
its turn, and is **re-engaged automatically when the command exits** (a
`task_notification` arrives in the still-open stream) — no new user message. Such
autonomous turns ride `/events` as `{ reply, solicited:false }`.

## HTTP API

| Method + path     | Purpose |
|-------------------|---------|
| `POST /prompt`    | Body `{ text }`. Queues a user turn in the session. Returns `202 { ok }` immediately — the reply + live progress arrive over `/events`, so the turn outlives this request. `409` if a turn is already running. |
| `GET  /events`    | SSE. `{ busy }` snapshot on connect, then `{ activity }` lines while the model works, then `{ reply, solicited }` / `{ error }` per turn. `solicited:false` = an autonomous (monitoring) message, not the answer to a `/prompt`. |
| `POST /abort`     | Interrupt the in-flight turn (`query.interrupt()`); the session stays alive. |
| `GET  /status`    | `{ busy, monitoring, sessionId }` for the dashboard poller. `monitoring` = a `computer-use wait-for-stuck` process is alive. |
| `GET  /health`    | `ok`. |

The session id is kept **in memory only**: a CoW clone boots a fresh wrapper, so
it naturally starts a brand-new conversation instead of inheriting the
template's history.

## Auth

Uses the container's logged-in `claude` subscription
(`~/.claude/.credentials.json`) — **no API key**. The SDK runs its bundled Claude
Code CLI under `node` (`AGENT_EXECUTABLE`).

## Config (environment)

| Var | Default | Notes |
|-----|---------|-------|
| `AGENT_PORT` | `4096` | listen port |
| `AGENT_MODEL` | `claude-opus-4-8` | Claude model id |
| `AGENT_EXECUTABLE` | `node` | JS runtime for the bundled CLI |
| `AGENT_CONTROL_MCP_URL` | `http://10.60.0.1:9000/mcp` | control-server MCP (`set_state`) |
| `COMPUTER_USE_BIN` | `/usr/local/bin/computer-use` | desktop MCP binary |
| `COMPUTER_USE_MAX_WIDTH` / `_HEIGHT` | unset | override the desktop MCP's screenshot cap; unset ⇒ its built-in 1080p default |
| `LINEAR_{WE,DEV,HH,PER}_API_KEY` | unset | per-workspace Linear hosted MCP; empty key ⇒ that server is skipped |

Instructions (the per-host "operating notes": coordinates, asking-the-human,
app quirks, the monitor loop) live in `operating-notes.md`, deployed once to the
container as `~/.claude/CLAUDE.md` (the wrapper does **not** write it — see Run /
deploy). Both consumers read it from there: the SDK agent via
`settingSources: ["user"]` (which also loads the user's own
`~/.claude/settings.json` — theme, etc.), and any interactive `claude` a human
opens in the container (auto-loads `~/.claude/CLAUDE.md`).
`permissionMode`/`mcpServers`/`model` are set programmatically and override
anything `settings.json` might contain.

The **"Implementing a ticket"** procedure (open Cursor, drive the Claude Code
panel, open Firefox to the ticket, monitor) lives in `ticket-procedure.md` and is
injected as a **system-prompt `append` for this one session agent only** — NOT in
`~/.claude/CLAUDE.md`. That's deliberate: CLAUDE.md is read by every `claude` on
the host, including the Claude Code inside Cursor that this agent types
`implement <link>` into; if the procedure were shared, that inner agent would
recursively try to open Cursor. The file is read at startup via `import.meta.url`,
so it must sit next to `src/` (it ships with the repo — no extra deploy step).

## Run / deploy

```sh
bun install
cp operating-notes.md ~/.claude/CLAUDE.md   # the agent's operating notes (re-copy on change)
bun run src/server.ts                        # local
```

Deployed as a user systemd unit — see `../tests/agent-wrapper.service`.
