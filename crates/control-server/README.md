# control-server

The backend binary — one tokio service that is the **control plane**, the **media plane**,
and the **fleet-automation plane**. It exposes **four ports** and is a single self-contained
artifact (the frontend, the `clone-daemon`/`agent-wrapper` binaries, and the patched
gnome-shell `.deb` are all embedded). Full references: [API](../../docs/API.md) ·
[MCP](../../docs/MCP.md) · [PROTOCOL](../../docs/PROTOCOL.md) · [DEPLOY](../../docs/DEPLOY.md).

| Port | Default | Transport | Serves |
|---|---|---|---|
| **1 — video** | 9001 | framed H.264/JSON over TCP | the native [viewer](../viewer/README.md): selected clone's monitors out; input/clipboard/cursor |
| **2 — web API** | 9000 | `axum` HTTP + SSE + embedded frontend | the [frontend](../../frontend/README.md): `/events`, all `/api/*`, the SPA |
| **3 — per-clone MCP** | 9002 | HTTP JSON-RPC (IP-routed) | the in-clone agent's `set_state` (clone resolved from caller IP) |
| **4 — fleet MCP** | 9003 | HTTP JSON-RPC | operator/fleet: web actions (local) + desktop/window tools (proxied to the clone's daemon MCP) |

## Modules

`app` (shared state holder) · `state` (in-memory `ControlState` + atomic `state.json` persist
+ file-watch + SSE bus) · `config` (load/merge/redact `config.json` at 0600) · `web` (port 2
routes + SSE + SPA) · `mediaplane` (port 1: clone-socket ingest → `media` encode → viewer;
input routing; clipboard broker) · `mcp` (ports 3 + 4) · `orchestrate` (SSH `pct`/`lvcreate`
scripts + Operation parse) · `jobs` (clone/delete/bootstrap Operation machine) · `linear` ·
`claude` (usage poll + token refresh/push + assign/swap) · `chat` (agent-wrapper proxy + per-host SSE) ·
`monitor` (host poller) · `mounts` (sshfs) · `files` (notes/uploads/detector-feedback) ·
`embed` (the gzipped clone-daemon/agent-wrapper/gnome-shell-deb).

## Port 1 — media plane (`mediaplane` → [media](../media/README.md))

Streams the **selected** clone's monitors, one H.264 stream per monitor over one TCP
connection (1-byte tag framing: video / clipboard / cursor / layout — see
[PROTOCOL.md](../../docs/PROTOCOL.md#port-1-viewer-protocol-viewer--control-server)). On
`state.selected` change it re-points at the new clone's daemon socket, renegotiates the
monitor set, and forces an IDR. Viewer input is relayed to the selected clone. control-server
is also the **clipboard broker**: it tracks the current owner and fans each `ClipboardOffer`
to the viewer **and every other clone** (remote↔local + remote↔remote), routing requests to
the owner and bytes back to the requester, re-binding as `selected` changes.

## Port 2 — web API

State store + SSE, all `/api/*` routes, the embedded SPA, and `/uploads`. Orchestration
(clone/delete/bootstrap over Proxmox SSH, Linear, Claude, chat proxy, monitor poller, sshfs
mounts). Every endpoint is documented in [API.md](../../docs/API.md). Config is edited via the
Settings UI: `GET /api/config` returns a redacted view, `PUT` merges + persists 0600 +
applies live, `POST /api/config/test` checks Proxmox SSH.

## Ports 3 & 4 — MCP (`mcp`)

Hand-rolled JSON-RPC-over-HTTP (curl-testable; not `rmcp`).
- **Port 3 (per-clone, IP-routed):** the one tool is `set_state` — the in-clone agent reports
  `working`/`idle` + a note; the clone is resolved from the caller's source IP.
- **Port 4 (fleet):** web-action tools (`list_hosts`, `select`, `clone`, `delete`, `redeploy`,
  `claude_*`, `set_state`) run locally; desktop/window tools (`screenshot`, `mouse_move`,
  clicks, `scroll`, `key`, `type`, `list_windows`, `move_window`, `list_apps`, `launch_app`)
  are **proxied** to the addressed clone's daemon MCP at `http://{host}:{daemon_mcp}`.

The full desktop-automation surface lives in the **clone-daemon** (`:9004`), not here — the
in-clone agent calls it directly on localhost and the fleet MCP proxies to it. Every tool +
args: [MCP.md](../../docs/MCP.md).

## Claude account assignment & swap (`claude`)

Each account has **two credentials**: a **refresh token** (+ cached short-lived access token)
used server-side **only** to read 5h/7d usage (429 backoff), never sent to a clone; and a
**long-lived token** that actually runs Claude Code. Delivery writes the clone's
`~/.claude/.credentials.json` (long-lived token, refresh **emptied** so the SDK never rotates
it) — read at request time, so a **running** clone hot-swaps with no restart. **Auto-assign**
at clone time by usage+load score; **hot-swap** from the UI/`/api/claude/swap`/fleet MCP;
**auto-swap** to the next-best account on exhaustion (`claude.auto_swap_on_exhaustion`).

## Orchestration & self-bootstrap (`orchestrate`, `jobs`)

Clone/delete/bootstrap run the embedded shell scripts over `ssh … bash -s` and parse the
`P step msg` / `RESULT …` line protocol into an `Operation` streamed over `/events`. Two clone
paths: **CoW** (`clone.sh`, fast, the default) and **from-zero bootstrap** (`bootstrap.sh` +
`provision-clone.sh`, used to build the golden template). The deployment promise: give the
control-server Proxmox SSH and it builds the template + provisions clones — distributing the
embedded `clone-daemon`, `agent-wrapper`, and patched gnome-shell deb. See
[DEPLOY.md](../../docs/DEPLOY.md) and [SCRIPTS.md](../../docs/SCRIPTS.md).

## Networking

Only the control-server needs external reachability (tailscale, manual). Clones sit on an
internal bridge reachable only *from* the control-server (SSH + the agent-wrapper chat proxy +
the fleet-MCP→daemon-MCP proxy); media/input cross a host-bind-mounted unix socket
(`/srv/rmng-sock`, SCM_RIGHTS), not the network. Exposure split: ports 1+2 operator-facing;
port 3 internal bridge only (needs real peer IPs); port 4 most-privileged (localhost/token).

## Dependencies

`axum`/`tokio`/`tower-http` (port 2 + the MCP HTTP servers + static files), `reqwest` (Linear,
Claude, agent-wrapper, the daemon-MCP proxy — plain HTTP, no rustls/native-tls), `rust-embed`
+ `flate2` (embedded frontend + binaries + deb), `notify` (file watch), `serde_json`,
`tokio::process` (ssh), `wire`, `media`.

## Tests

`cargo test -p control-server` (run on the build CT — the crate links GStreamer): Operation
state machine from canned `P…`/`RESULT…`, account scoring, config defaults/merge/redaction,
source-IP→clone mapping, and the embed round-trip (the patched deb decompresses to a valid
`.deb`).
