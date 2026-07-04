# HTTP API reference — web port (default `:9000`)

The control-server's **port 2** serves the React management UI (a static SPA served from
disk), the JSON control API, and two SSE streams. It binds `0.0.0.0:{listen.web}`.

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
| GET | `/events` | Global state SSE (snapshot + diffs) + named `stats` + `forwards` events | 200 SSE `ControlState` |
| POST | `/api/activate` | Select the host shown in the viewer | 200 `ControlState` |
| POST | `/api/reorder` | Reorder the host list | 200 `ControlState` |
| PUT | `/api/hosts/:id/forwards` | Replace a host's port-forward rules | 200 `ControlState` |
| POST | `/api/clone` | Start a clone from an image (Linear ticket / new ticket / plain) | 200 `{ok, op}` |
| POST | `/api/layout/activate` | Make a layout preset active and live-apply it to all running clones | 200 `{ok,applied,errors}` |
| POST | `/api/delete` | Destroy a clone / unregister a plain host | 200 `Operation` |
| GET | `/api/setup/env` | Setup wizard environment preflight rows | 200 `SetupEnv` |
| GET | `/api/images` | List clone-source images (`rmng.image=1`) | 200 `ImageInfo[]` |
| POST | `/api/images/pull` | Pull the clone template from a registry (keeps its own `repo:tag`) | 200 `Operation` |
| POST | `/api/images/commit` | Commit a running clone to a new image | 200 `Operation` |
| POST | `/api/images/delete` | Remove a clone-source image | 200 `{ok}` |
| GET | `/api/notes/:id` | Fetch a host's rich-text notes | 200 `[block]` |
| POST | `/api/notes/:id` | Save a host's notes | 204 |
| POST | `/api/upload` | Upload an image (multipart) | 200 `{url}` |
| GET | `/uploads/:file` | Serve an uploaded image | 200 binary |
| POST | `/api/detector-feedback` | Clone reports a wrong needs-human verdict (multipart) | 200 `{ok,id,host}` |
| GET | `/api/config` | Current config, secrets redacted | 200 `AppConfigRedacted` |
| PUT | `/api/config` | Merge a partial config update (persists 0600) | 200 `{ config, restartRequired, networkWarning? }` |
| POST | `/api/config/test` | Test a setting (currently `"docker"`) | 200 `{ok,message}` |
| POST | `/api/claude/import/check` | Check a clone is signed in via claude.ai | 200 `{ok,email,orgName,subscriptionType}` |
| POST | `/api/claude/import` | Import a Claude account from a signed-in clone | 200 `{ok,email,cleared}` |
| POST | `/api/claude/refresh` | Force one usage poll now | 200 `{ok,rateLimited}` |
| POST | `/api/claude/swap` | Change a clone's Claude account/group (email/`auto`/`group:<name>`/`none`) | 200 `{ok,account,group,selection}` |
| POST | `/api/claude/rotate` | Run one group-rotation pass now | 200 `{ok}` |
| POST | `/api/codex/import/check` | Check a clone is signed in via ChatGPT | 200 `{ok,email,plan,accountId}` |
| POST | `/api/codex/import` | Import a Codex account from a signed-in clone | 200 `{ok,email,cleared}` |
| POST | `/api/codex/refresh` | Force one Codex usage poll now | 200 `{ok,rateLimited}` |
| POST | `/api/codex/swap` | Change a clone's Codex account/group | 200 `{ok,account,group,selection}` |
| POST | `/api/codex/rotate` | Run one Codex group-rotation pass now | 200 `{ok}` |
| GET | `/api/chat/:id` | Chat snapshot for a host | 200 `ChatSnapshot` |
| POST | `/api/chat/:id` | Send a message to the host's agent | 202 |
| GET | `/api/chat/:id/events` | Per-host chat SSE | 200 SSE `ChatSnapshot` |
| POST | `/api/chat/:id/abort` | Abort the in-flight agent turn | 204 |
| GET | `/*` | SPA fallback (embedded frontend) | 200 asset / `index.html` |

Error statuses: `400` validation, `404` unknown id/file, `409` chat busy / image still in
use, `500` server (I/O), `502` the Docker daemon or agent-wrapper is unreachable. Error bodies
are a plain string or `{error}`.

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
| `monitors` | `MonitorSpec[]` | legacy field, kept for JSON back-compat; no longer populated (always `[]`) — use `activeLayout` + the config's `layoutPresets` |
| `activeLayout` | `string` | name of the active layout preset, mirrored from config so the sidebar switcher updates over SSE |
| `layoutPresetNames` | `string[]` | names of all layout presets, in config order — drives the sidebar's segmented preset buttons |
| `hosts` | `Host[]` | all registered clones + plain hosts |
| `operations` | `Operation[]` | in-flight + recent clone/delete/pull/commit jobs |
| `claude_accounts` | `ClaudeUsage[]` | per-account 5h/7d usage + spend |

`Host` carries connection info (`id`, `host`, `port`, `username`, …), the `managed` flag
(true = a Docker container named after the host id backs it; false = a plain unmanaged
row), the `source` image reference, the assigned
`claude_account_email`, Linear metadata (`linear_workspace`, `linear_ticket`, `linear_branch`,
…), `agent_report` (working/idle), `state_note`, `monitor_state` (working/idle/offline), and
`forwards` (`PortForward[]` — the host's persisted port-forward rules; live status rides the
`forwards` SSE event below, never `ControlState`).
`Operation` carries `id`, `kind` (clone/delete/pull/commit — a persisted legacy `"bootstrap"`
op still loads, aliased onto `pull`), `target`, `source`, `status`, `step`, `pct`, a rolling
`log`, and timestamps.

### `stats` event
The same `/events` connection multiplexes a second, named SSE event: `stats`, a live
`{ <hostId>: ContainerStats }` map for running **managed** clones only (a stopped or
unmanaged host contributes no entry). `ContainerStats` ([control.rs](../crates/wire/src/control.rs)):
`cpuPct` (percentage of ONE core — 100 == a single fully-used core, so a container busy
across several cores reads > 100; docker-CLI convention), `memUsed`/`memLimit` (bytes,
docker-CLI semantics). Sampled by the monitor poller alongside its 4 s `/status` probe
([monitor.rs](../crates/control-server/src/monitor.rs)); a new subscriber gets the latest
map immediately, then one push per tick — but only when the map actually changed (deduped
by value, not serialization, so an idle fleet doesn't wake subscribers). Deliberately kept
out of `ControlState`/`state.json`: these numbers move every tick, and every `ControlState`
mutation persists the file, so folding stats in would rewrite it every 4 s.

### `forwards` event
The same `/events` connection multiplexes a third, named SSE event: `forwards`, the volatile
port-forward **runtime** map — the live status of each host's forward rules as the viewer
opens/closes its local listeners. A new subscriber gets the current snapshot immediately, then
one push per status change. It rides its own SSE-only bus (`crate::forward::ForwardBus`), so —
like `stats` — it never enters `ControlState`/`state.json`. The *desired* rules themselves are
persisted on `Host.forwards` and edited via `PUT /api/hosts/:id/forwards`.

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
Start a clone container from a clone-source image. Runs async — returns an `Operation` id
immediately; progress flows over `/events`. After the clone is up the server kicks off the
agent's first message ([chat::kickoff_agent](../crates/control-server/src/chat.rs)).

Body (one of three task modes + optional account/instructions):
```jsonc
{
  "image": "pegasis0/rmng-template:latest", // required: clone-source image reference (from GET /api/images)
  // -- pick ONE task mode --
  "ticket": "DEV-123",              // existing Linear ticket, OR
  "create": { "team": "dev", "title": "...", "description": "..." },      // new ticket, OR
  "plain":  { "title": "quick task", "message": "do X" },                 // no ticket
  // -- optional --
  "preset": "<name>" | "auto",      // clone preset (env + Linear key). Ticket mode:
                                    //   absent/"auto" auto-selects by the ticket's labels
                                    //   (400 listing them if nothing matches). Plain mode:
                                    //   REQUIRED while any presets exist. Create mode:
                                    //   REQUIRED (the preset's key creates the ticket).
  "claudeAccount": "user@anthropic.com" | "auto" | "group:<name>" | "none",
  "codexAccount":  "user@openai.com"  | "auto" | "group:<name>" | "none",
  "agentInstructions": "...",       // extra context for the agent-wrapper
  "claudeInstructions": "..."       // extra instructions for Claude Code
}
```
`image` accepts a `repo:tag` reference (e.g. `pegasis0/rmng-template:latest`), a full `sha256:…` id, or a bare 64-hex id;
whatever form is passed is canonicalized to the reference and recorded on the host as
`source`. The image must carry the `rmng.image=1` label (a raw non-image id is rejected). The
selected preset's vars are written into the clone's session env, plus `LINEAR_API_KEY=<preset
key>` (auths the clone's `linear` MCP). Hostname is derived (`pega-{ticket}` or a slug of the
plain title, with a numeric suffix on collision). Returns `{ "ok": true, "op": Operation }` or
`400 {error}`.

> There is no `/api/clone/redeploy` endpoint (and no fleet MCP `redeploy` tool) any more.
> Clone binaries (`clone-daemon`/`agent-wrapper`) hot-swap themselves automatically — a hash
> check on the daemon's `Hello` plus a periodic sweep — see
> [DEPLOY.md#upgrades](DEPLOY.md#upgrades).

### `POST /api/layout/activate` — body `{ "name": string }`
Make the named layout preset the active one and live-apply it to every running clone — no
session restart, no app loss. Validates `name` against `config.layoutPresets` (`400` if
unknown), persists it as `config.activeLayout`, mirrors `activeLayout` +
`layoutPresetNames` into `ControlState` (SSE), then pushes `ServerMsg::SetMonitors` with the
preset's monitors to every connected clone-daemon over the clone socket. Each daemon does a
make-before-break session swap (builds a fresh Mutter session with the new monitors, switches
capture + input to it, stops the old one) — running apps never close. Returns
`{ "ok": bool, "applied": string[], "errors": string[] }`. `ok` is currently always `true`
(the activation itself succeeded; per-clone results are in `applied`/`errors`). `applied`/
`errors` cover only clones whose daemon is currently connected: a daemon whose connection has
already dropped is silently skipped (absent from both lists), since the server only pushes to
currently-connected daemons. `errors` captures an immediate socket-send failure only (there is
no ack).

### `POST /api/delete` — body `{ "id": string }`
Destroy a managed clone (stops it with `SIGRTMIN+3`, removes the container and its
`rmng-dind-<id>` inner-Docker volume) or unregister a plain host. Returns the `Operation`;
progress over `/events`.

---

## Images (clone-source templates) & setup

Clone sources are images labeled `rmng.image=1`, identified by their own `repo:tag` (e.g.
`pegasis0/rmng-template:latest`) — there is no local retag and no golden-CT / CoW model. `POST`
bodies (references contain `/` and `:`, so nothing uses path params).

### `GET /api/images` → `ImageInfo[]`
List clone-source images, newest first. Each `ImageInfo` carries `id` (`sha256:…`),
`reference` (the image's own `repo:tag`, e.g. `pegasis0/rmng-template:latest`), `size_bytes`,
`created_at`, `base` (true for the published clone template, `rmng.base=1`), `created_from`
(lineage, `rmng.created-from`), and `in_use_by` (host ids of live clones whose `source` is this
image). `502` if the daemon is unreachable.

### `POST /api/images/pull` — body `{ "reference"?: string }`
Pull the clone template from a registry. The pulled image keeps its own `repo:tag` as the
clone-source reference — no local retag. `reference` is a registry `repo:tag` to pull from —
absent/blank defaults to `config.docker.templateReference` (default
`pegasis0/rmng-template:latest`); see
[DEPLOY.md#publishing-the-template](DEPLOY.md#publishing-the-template) for how that image is
built and published. Rejects a blank reference, a `repo@sha256:…` digest reference (pull a
`repo:tag` instead), and a duplicate pull already in flight for the same reference. Verifies
the pulled image carries the `rmng.image=1` label (else it isn't an RMNG template) and warns —
without refusing — if its `StopSignal` isn't `SIGRTMIN+3`. Re-pulling the same `repo:tag`
naturally moves the local tag onto the fresh image (standard `docker pull`) — that is the
refresh. Returns the driving `Operation` (kind `pull`, which the setup wizard's "Download
template" step watches for, showing aggregate byte progress). Replaces the retired in-product
`/api/images/bootstrap` build — no base OS is built in-product any more, only pulled pre-built.

### `POST /api/images/commit` — body `{ "host": string, "name": string }`
Commit a running managed clone (`host`) to a new clone-source image `<name>:latest` — the
DNS-label `name` is the full repo (kind `commit`). `docker commit` **excludes volume mounts**,
so the clone's inner-Docker state (`/var/lib/docker`) never enters the image — clones always
start with an empty inner Docker. On-disk credentials in the clone's home **are** baked in
(logged as a warning). Rejects a name that already exists.

### `POST /api/images/delete` — body `{ "reference": string }` → `{ok}`
Remove a clone-source image. `409` if any host still runs on it (`in_use_by` non-empty) or a
running operation (clone/commit/pull) references it as its source or target; the daemon's own
"in use by a container" `409` is surfaced too. If the same image carries more than one tag,
deleting one `reference` only untags it while the others stay attached to the same layers — the
image re-lists under a remaining reference; delete again to actually free them.

### `GET /api/setup/env` → `SetupEnv`
The setup wizard's environment preflight: `{ rows: EnvCheckRow[] }`, each row `{ id, label,
ok, detail, required }`. Rows, in order: **Docker daemon** reachable (`dockerDaemon`,
required), **control-server container** detected (`selfContainer`, info — absence = dev mode),
**clone media socket mount** at `/srv/rmng-sock` (`sockMount`, required), **GPU render node**
`/dev/dri/renderD128` (`renderNode`, required), and **LXCFS** on the Docker host (`lxcfs`,
advisory — without it clones see host-wide `/proc` values). Cached from the Docker self-setup probe
(refreshed at startup and by `POST /api/config/test {docker}`).

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
was wrong. The caller **self-identifies** with a `clone` field (its hostname == host id —
clone IPs are dynamic Docker IPAM, so there is no source-IP mapping). Fields: `clone`
(required), `kind` (`false-positive`|`false-negative`, required), `detectorVerdict`,
`detectorReason`, `actualState`, repeated `ignoreReason`, `note`, and an optional
`screenshot` file. Persists a JSON record + screenshot under
`data/detector-feedback/`. Returns `{ "ok": true, "id": "...", "host": "..." }`.

---

## Configuration

### `GET /api/config` → `AppConfigRedacted`
The full config with the only secret (preset Linear keys) replaced by `linearKeySet: bool`.
Everything else is returned verbatim — ports, `layoutPresets`/`activeLayout`, the `docker` block
(`socket`/`subnet`/`hostnamePrefix`/`cloneCpus`/`cloneMemoryMb`; no secret — the local daemon
socket needs none), `staticDir`/`cloneSocket`/`chroma`, `setupComplete`, `detectorInferenceUrl`,
`agentPlaybook` (the editable agent playbook seeded with the shipped default and injected into new
clones — non-secret; a preset's optional `agentPlaybook` append rides along in each `presets` row),
and claude poll config. See [PROTOCOL.md](PROTOCOL.md#config-schema) for the schema.

### `PUT /api/config` (partial merge) → `{ config, restartRequired, networkWarning? }`
Deep-merge a partial config over the stored one, persist to disk at `0600`, apply live.
Returns the redacted config plus `restartRequired: boolean` — set when a restart-required
field changed (the four listen ports, `cloneSocket`, `docker.socket`, `staticDir`, `chroma`)
so the UI can prompt for a restart. `cloneSocket` still triggers this pre-latch (the server
bound the old path at startup) even though it is a one-time field (see below). A wizard-finish
flip (`setupComplete` false → true) materializes the lazy `rmng` network here; a failure is
non-fatal and echoed as `networkWarning`. Merge rules: an **empty string keeps** the stored
value; a non-empty string replaces it; `presets` rows merge by name (blank `linearKey` keeps
the stored one). `docker.subnet` is validated as an IPv4 `/16`–`/24` CIDR. One-time fields
(`dataDir`, `cloneSocket`, `docker.subnet`) are locked once `setupComplete` latches (which
itself is a one-way latch).

### `POST /api/config/test` — body `{ "what": "docker" }` → `{ ok, message }`
Synchronously test a setting. Currently only `"docker"`: re-runs the Docker self-setup probe
and collapses the environment report (daemon reachable, sock mount, render node) into a single
`(ok, message)` verdict. The row-by-row breakdown is `GET /api/setup/env`.

---

## Claude accounts

| Endpoint | Body | Returns | Does |
|---|---|---|---|
| `POST /api/claude/import/check` | `{host}` | `{ok, email, orgName, subscriptionType}` | Run `claude auth status` in the clone; require a claude.ai login and return its identity |
| `POST /api/claude/import` | `{host}` | `{ok, email, cleared}` | Harvest the clone's OAuth pair (read off its disk) into the server's secret store, then delete the clone's credentials file |
| `POST /api/claude/refresh` | — | `{ok, rateLimited}` | Force one usage poll; `rateLimited` if any account hit 429 |
| `POST /api/claude/swap` | `{host, account}` | `{ok, account, group, selection}` | Resolve `account` (email / `auto` / `group:<name>` / `none`) and write the clone's `~/.claude/.credentials.json` via `docker exec`. A `group:` selection binds the clone to that group for rotation; `none` removes the credentials file (`account` null); the verbatim choice is echoed as `selection` and stored on the host (`502` if unreachable) |
| `POST /api/claude/rotate` | — | `{ok}` | Run one group-rotation pass immediately (the rotator otherwise runs every 10 min). Sticky: a clone keeps its account while it stays eligible (member, imported, 5h usage ≤ 90%); only clones whose account fell out of eligibility move, to the least-loaded / least-used member |

The single-token model (the server owns each account's OAuth pair and pushes the current
short-lived access token to assigned clones on every refresh) is described in
[PROTOCOL.md](PROTOCOL.md#claude-accounts).

---

## Codex accounts

Codex (OpenAI/ChatGPT) accounts mirror the Claude endpoints. The server owns each
account's OAuth pair; clones receive only a short-lived injected `~/.codex/auth.json`.

| Endpoint | Body | Returns | Does |
|---|---|---|---|
| `POST /api/codex/import/check` | `{host}` | `{ok, email, plan, accountId}` | Confirms the clone is signed in to Codex via ChatGPT (reads `~/.codex/auth.json`, decodes the id_token JWT). Errors if signed in with an API key |
| `POST /api/codex/import` | `{host}` | `{ok, email, cleared}` | Harvests the OAuth triple, stores it (0600 `codex-accounts.json`), clears the clone's auth.json |
| `POST /api/codex/refresh` | — | `{ok, rateLimited}` | Force one usage poll; `rateLimited` if any account hit 429 |
| `POST /api/codex/swap` | `{host, account}` | `{ok, account, group, selection}` | Resolve `account` (email / `auto` / `group:<name>` / `none`) and write the clone's `~/.codex/auth.json` via `docker exec`. A `group:` selection binds the clone to that group for rotation; `none` removes the auth file; the verbatim choice is echoed as `selection` and stored on the host (`502` if unreachable) |
| `POST /api/codex/rotate` | — | `{ok}` | Run one Codex group-rotation pass immediately |

Clone creation (`POST /api/clone`) accepts an optional `codexAccount` alongside
`claudeAccount`; a clone can be assigned both independently.

The single-token model (the server owns each account's OAuth pair and pushes the current
short-lived access token to assigned clones on every refresh) is described in
[PROTOCOL.md](PROTOCOL.md#codex-accounts).

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
Serves the installed React build from disk; unknown paths fall back to `index.html` for
client-side routing. The bundle is resolved at startup: `/usr/local/share/rmng/static` in the
image, else the repo dev build (`frontend/build/client`). A non-empty `staticDir` config field
(Settings → Advanced; restart-required) overrides that with any disk path (frontend
hot-reload during dev). If no frontend resolves anywhere, the route returns a 404 hint and the
API stays up.
