# HTTP API reference — web port (default `:9000`)

The control-server's **port 2** serves the React management UI (embedded SPA), the
JSON control API, and two SSE streams. It binds `0.0.0.0:{listen.web}` with
`ConnectInfo::<SocketAddr>` (the source IP is used by `/api/detector-feedback`).

- **No auth.** All endpoints are open; the server is meant to sit behind a tailnet /
  firewall. Path params that hit the filesystem (`notes`, `uploads`) are validated as
  DNS labels / `<hex>.<ext>` to prevent traversal.
- **Source files:** routes + handlers in [crates/control-server/src/web.rs](../crates/control-server/src/web.rs);
  chat handlers in [chat.rs](../crates/control-server/src/chat.rs); persisted shapes in
  [files.rs](../crates/control-server/src/files.rs); wire types in
  [crates/wire/src/control.rs](../crates/wire/src/control.rs) and [config.rs](../crates/wire/src/config.rs).
- All request/response bodies are JSON unless noted (uploads/feedback are `multipart/form-data`).
- The frontend talks to this port using ts-rs-generated types in `frontend/app/lib/wire/`,
  kept byte-compatible with the Rust `wire` types.

## Endpoint summary

| Method | Path | Purpose | Success |
|---|---|---|---|
| GET | `/events` | Global state SSE (snapshot + diffs) | 200 SSE `ControlState` |
| POST | `/api/activate` | Select the host shown in the viewer | 200 `ControlState` |
| POST | `/api/reorder` | Reorder the host list | 200 `ControlState` |
| POST | `/api/clone` | Start a CoW clone (Linear ticket / new ticket / plain) | 200 `{ok, op}` |
| POST | `/api/template/bootstrap` | Build a fresh template/clone from the base image | 200 `Operation` |
| POST | `/api/clone/redeploy` | Hot-swap a clone's daemon (+agent) binaries | 200 `{ok}` |
| POST | `/api/monitors/apply` | Push the saved monitor layout to all running clones | 200 `{ok,applied,errors}` |
| POST | `/api/delete` | Destroy a clone / unregister a plain host | 200 `Operation` |
| GET | `/api/notes/:id` | Fetch a host's rich-text notes | 200 `[block]` |
| POST | `/api/notes/:id` | Save a host's notes | 204 |
| POST | `/api/upload` | Upload an image (multipart) | 200 `{url}` |
| GET | `/uploads/:file` | Serve an uploaded image | 200 binary |
| POST | `/api/detector-feedback` | Clone reports a wrong needs-human verdict (multipart) | 200 `{ok,id,host}` |
| GET | `/api/config` | Current config, secrets redacted | 200 `AppConfigRedacted` |
| PUT | `/api/config` | Merge a partial config update (persists 0600) | 200 `AppConfigRedacted` |
| POST | `/api/config/test` | Test a setting (currently Proxmox SSH) | 200 `{ok,message}` |
| POST | `/api/claude/import` | Import accounts from the template's claude-swap dir | 200 `{ok,imported}` |
| POST | `/api/claude/refresh` | Force one usage poll now | 200 `{ok,rateLimited}` |
| GET | `/api/claude/recommended` | Recommended account for a new clone | 200 `{email}` |
| POST | `/api/claude/swap` | Hot-swap a clone's Claude account | 200 `{ok,account}` |
| GET | `/api/chat/:id` | Chat snapshot for a host | 200 `ChatSnapshot` |
| POST | `/api/chat/:id` | Send a message to the host's agent | 202 |
| GET | `/api/chat/:id/events` | Per-host chat SSE | 200 SSE `ChatSnapshot` |
| POST | `/api/chat/:id/abort` | Abort the in-flight agent turn | 204 |
| GET | `/*` | SPA fallback (embedded frontend) | 200 asset / `index.html` |

Error statuses: `400` validation, `404` unknown id/file, `409` chat busy, `500` server
(I/O, SSH), `502` agent-wrapper unreachable. Error bodies are a plain string or `{error}`.

---

## State & SSE

### `GET /events`
Subscribe to all control-state changes. Emits a full `ControlState` JSON snapshot
immediately, then a fresh snapshot on every `store.mutate()`; a `ping` comment every 20 s
keeps the connection alive. This is what the dashboard subscribes to.

`ControlState` ([control.rs](../crates/wire/src/control.rs)):

| Field | Type | Notes |
|---|---|---|
| `selected` | `string?` | host id shown in the viewer |
| `monitors` | `MonitorSpec[]` | global desired layout |
| `hosts` | `Host[]` | all registered clones/templates/plain hosts |
| `operations` | `Operation[]` | in-flight + recent clone/delete jobs |
| `templates` | `string[]` | host ids that are templates |
| `claude_accounts` | `ClaudeUsage[]` | per-account 5h/7d usage + spend |

`Host` carries connection info (`id`, `host`, `port`, `username`, …), the Proxmox `ctid`,
the assigned `claude_account_email`, Linear metadata (`linear_workspace`, `linear_ticket`,
`linear_branch`, …), `agent_report` (working/idle), `state_note`, and `monitor_state`
(working/idle/offline). `Operation` carries `id`, `kind` (clone/delete), `target`,
`status`, `step`, `pct`, a rolling `log`, `ctid`, and timestamps.

---

## Host selection & ordering

### `POST /api/activate` — body `{ "id": string | null }`
Set `selected` (or clear with `null`). Returns the updated `ControlState`. The media plane
re-targets port 1 to the newly selected clone.

### `POST /api/reorder` — body `{ "order": string[] }`
Reorder `hosts` by the given list of ids. Returns the updated `ControlState`.

---

## Clone lifecycle

### `POST /api/clone`
Start a Copy-on-Write clone from a source host. Runs async — returns an `Operation` id
immediately; progress flows over `/events`. After the clone is up the server kicks off the
agent's first message ([chat::kickoff_agent](../crates/control-server/src/chat.rs)).

Body (one of three task modes + optional account/instructions):
```jsonc
{
  "source": "rmng-template",          // required: source host id
  // -- pick ONE task mode --
  "ticket": "DEV-123",              // existing Linear ticket, OR
  "create": { "workspace": "dev", "title": "...", "description": "..." }, // new ticket, OR
  "plain":  { "title": "quick task", "message": "do X" },                 // no ticket
  // -- optional --
  "claudeAccount": "user@anthropic.com" | "auto",
  "agentInstructions": "...",       // extra context for the agent-wrapper
  "claudeInstructions": "..."       // extra instructions for Claude Code
}
```
Hostname is derived (`pega-{ticket}` or a slug of the plain title, with a numeric suffix on
collision). Returns `{ "ok": true, "op": Operation }` or `400 {error}`.

### `POST /api/template/bootstrap` — body `{ "hostname": string }`
Build a brand-new container from `config.template.base_image` (full provisioning, not CoW):
headless GNOME + clone-daemon + agent-wrapper + the patched gnome-shell deb. Returns the
`Operation`. Use this once to create the golden template; thereafter clone from it.

### `POST /api/clone/redeploy` — body `{ "id": string, "daemonOnly"?: bool }`
Hot-swap a running clone's `clone-daemon` (+ `agent-wrapper` unless `daemonOnly`) from the
control-server's embedded copies, without reprovisioning (~10 s). The daemon reconnects to
the media socket; with `daemonOnly` the Claude session stays alive. Returns `{ok}`.

### `POST /api/monitors/apply`
Push `config.monitors` to every running clone (those with a `ctid`): rewrites each clone's
`RMNG_MONITORS` + dummy mode specs and restarts its GNOME + daemon. Returns
`{ "ok": bool, "applied": string[], "errors": string[] }` (partial success allowed).

### `POST /api/delete` — body `{ "id": string }`
Destroy a managed clone (stops + removes the CT and its thin snapshot) or unregister a
plain host. Returns the `Operation`; progress over `/events`.

---

## Notes & uploads

### `GET /api/notes/:id` → `[block]` &nbsp;·&nbsp; `POST /api/notes/:id` (204)
Per-host rich-text notes (BlockNote block array), stored at `data/notes/{id}.json`. `:id`
must be a DNS label. GET returns `[]` if none.

### `POST /api/upload` (multipart `file`) → `{ "url": "/uploads/<hex>.<ext>" }`
Image upload (png/jpeg/gif/webp/svg/avif/bmp, ≤15 MB) → `data/uploads/`.

### `GET /uploads/:file`
Serve an uploaded image by its generated `<16-hex>.<ext>` name, with the right Content-Type.

---

## Detector feedback

### `POST /api/detector-feedback` (multipart)
A clone's `clone-daemon report-detection` posts here when the needs-human detector's verdict
was wrong. The **caller's source IP** is matched against `hosts[].host` to resolve the clone
(so no auth/clone arg is needed). Fields: `kind` (`false-positive`|`false-negative`,
required), `detectorVerdict`, `detectorReason`, `actualState`, repeated `ignoreReason`,
`note`, and an optional `screenshot` file. Persists a JSON record + screenshot under
`data/detector-feedback/`. Returns `{ "ok": true, "id": "...", "host": "..." }`.

---

## Configuration

### `GET /api/config` → `AppConfigRedacted`
The full config with secrets replaced by `*_set: bool` booleans (Proxmox SSH, Linear keys,
clone-account tokens). Non-secret fields (ports, subnet, mac prefix, monitors, claude poll
config, template params) are returned verbatim. See [PROTOCOL.md](PROTOCOL.md#config-schema)
for the schema.

### `PUT /api/config` (partial merge) → `AppConfigRedacted`
Deep-merge a partial config over the stored one, persist to disk at `0600`, apply live.
Secret-merge rules: an **empty string keeps** the stored secret; a non-empty string replaces
it; `cloneAccounts` merge by `email` and blank tokens preserve stored ones.

### `POST /api/config/test` — body `{ "what": "proxmox" }` → `{ ok, message }`
Synchronously test a setting. Currently only `"proxmox"` (runs `ssh -o BatchMode=yes
-o ConnectTimeout=10 <target> true`).

---

## Claude accounts

| Endpoint | Body | Returns | Does |
|---|---|---|---|
| `POST /api/claude/import` | — | `{ok, imported}` | Scan the template host's mounted `~/.local/share/claude-swap` and import accounts |
| `POST /api/claude/refresh` | — | `{ok, rateLimited}` | Force one usage poll; `rateLimited` if any account hit 429 |
| `GET /api/claude/recommended` | — | `{email}` | Pinned account, else lowest-usage; `null` if none |
| `POST /api/claude/swap` | `{host, account}` | `{ok, account}` | Resolve `account` (email/`auto`) and write the clone's `~/.claude/.credentials.json` to hot-swap a running clone (`502` if unreachable) |

The two-token model (short-lived+refresh for usage polling; long-lived for running Claude
Code) is described in [PROTOCOL.md](PROTOCOL.md#cloneaccount).

---

## Per-host agent chat

The control-server proxies chat to each clone's agent-wrapper (`http://{host}:{agent_port}`,
default `:4096`), persisting history at `data/chats/{id}.json`.

| Endpoint | Body | Returns | Does |
|---|---|---|---|
| `GET /api/chat/:id` | — | `ChatSnapshot` | `{busy, activity, messages[]}` snapshot |
| `POST /api/chat/:id` | `{text}` | `202` / `409` if busy | Persist the user message, set busy, spawn the turn (opens the wrapper's `/events`, POSTs `/prompt`, relays activity, records the reply). Watchdog: 30 min hard / 3 min idle |
| `GET /api/chat/:id/events` | — | SSE `ChatSnapshot` | Snapshot + a fresh one on each message/activity/busy change; 20 s ping |
| `POST /api/chat/:id/abort` | — | `204` | Best-effort POST to the wrapper's `/abort`; clears busy |

`ChatMessage` = `{ id, role (user|assistant), text, ts }`.

---

## SPA fallback

### `GET /*`
Serves the embedded React build (`frontend/build/client` via `rust-embed`); unknown paths
fall back to `index.html` for client-side routing. Set `RMNG_STATIC_DIR=<dir>` to serve
from disk instead (frontend hot-reload during dev).
