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
| `LINEAR_API_KEY` | unset | Linear hosted MCP (registered as `linear`); injected per-clone from the chosen preset; empty ⇒ skipped |
| `AGENT_INSTRUCTIONS_PATH` | `~/.config/rmng/agent-instructions.md` | the editable agent playbook the control-server injects at clone creation; present + non-empty ⇒ wins over the baked-in default. Read once at startup |

The agent's instructions come in two layers:

**System-prompt `append` (this session agent only)** — the desktop **operating
notes** (coordinates, asking-the-human, app quirks, the monitor loop) and the
**"Implementing a ticket"** procedure (open Cursor, drive the Claude Code panel, open
Firefox to the ticket, monitor), now merged into a single **`agent-instructions.md`**.
It resolves at startup (`resolveSystemAppend`) from one of two sources:

- **Injected, Settings-editable (wins)** — at clone creation the control-server
  writes the effective playbook to `AGENT_INSTRUCTIONS_PATH` (default
  `~/.config/rmng/agent-instructions.md`). That playbook is composed *control-side* as
  the global `agentPlaybook` config (seeded with this file's shipped default) plus the
  chosen preset's optional per-preset append (concatenated after a blank line). When
  the file is present and non-empty it wins over the fallback.
- **Baked-in fallback** — `agent-wrapper/agent-instructions.md`, a `with { type:
  "text" }` import (`BAKED_IN_INSTRUCTIONS`) so it ships inside the `bun build
  --compile` single-exec (no deploy step). Used when the injected file is absent or
  empty (e.g. a local `bun run`).

Either way it is read **once at startup** and injected as the SDK's system-prompt
`append` for this session agent only. It is deliberately kept OUT of
`~/.claude/CLAUDE.md`, because that file is read by *every* `claude` on the host —
including the Claude Code inside Cursor that this agent types `implement <link>` into;
if the ticket procedure were shared, that inner agent would recursively try to open
Cursor.

**Shared on-disk memory** — `~/.claude/CLAUDE.md`: general engineering guidance
(disposable sandbox, verify-before-done, git discipline), written into the template image by
`template/setup/30-user.sh` at template build. Read by all three consumers: the SDK agent via
`settingSources: ["user"]` (which also loads `~/.claude/settings.json` — theme,
etc.), the inner Cursor Claude Code, and any interactive `claude` a human opens.
`permissionMode`/`mcpServers`/`model` are set programmatically and override anything
`settings.json` might contain.

## Run / deploy

```sh
bun install
bun run src/server.ts   # local (reads your own ~/.claude/CLAUDE.md as user memory)
```

Deployed as a user systemd unit — see `../tests/agent-wrapper.service`.
