# control-server

The backend binary — one tokio service that is the **control plane**, the **media plane**,
and the **fleet-automation plane**. It exposes **five listen ports** (9000 web, 9001 video,
9002 per-clone MCP, 9003 fleet MCP, 9005 forward) plus an SMB clone-home share on 445, and
ships as a Docker image; the
frontend and the `clone-daemon`/`agent-wrapper` binaries are plain on-disk payloads under
`/usr/local/share/rmng/` (read at runtime, hot-swapped into running clones) — nothing is
compiled into the binary. Clones themselves are created from a separately-published **template**
image (`pegasis0/rmng-template`, built by `template/Dockerfile`), pulled by `POST
/api/images/pull` — not built in-product, so the patched gnome-shell `.deb` isn't a
control-server payload at all. Full references: [API](../../docs/API.md) ·
[MCP](../../docs/MCP.md) · [PROTOCOL](../../docs/PROTOCOL.md) · [DEPLOY](../../docs/DEPLOY.md).

| Port | Default | Transport | Serves |
|---|---|---|---|
| **1 — video** | 9001 | framed H.264/JSON over TCP | the native [viewer](../viewer/README.md): selected clone's monitors out; input/clipboard/cursor |
| **2 — web API** | 9000 | `axum` HTTP + SSE + embedded frontend | the [frontend](../../frontend/README.md): `/events`, all `/api/*`, the SPA |
| **3 — per-clone MCP** | 9002 | HTTP JSON-RPC (header-routed) | the in-clone agent's `set_state` (clone self-identifies via the `x-rmng-clone` header) |
| **4 — fleet MCP** | 9003 | HTTP JSON-RPC | operator/fleet: web actions (local) + desktop/window tools (proxied to the clone's daemon MCP) |
| **5 — forward** | 9005 | framed TCP over TCP | the viewer's port-forward data plane: one TCP conn per accepted local socket, spliced to the clone |
| **SMB** | 445 | SMB (smbd) | the `clones` share — browse every running clone's `/home/rmng` from `smb://<host>/clones` (fixed cred `rmng`/`rmng`) |

## Modules

`app` (shared state holder) · `state` (in-memory `ControlState` + atomic `state.json` persist
+ file-watch + SSE bus) · `config` (load/merge/redact `config.json` at 0600) · `web` (port 2
routes + SSE + SPA) · `mediaplane` (port 1: clone-socket ingest → `media` encode → viewer;
input routing; clipboard broker) · `mcp` (ports 3 + 4) · `forward` (port-forward data plane:
viewer TCP spliced to the clone) · `docker` (bollard primitives against
the local daemon) · `provision` (clone/pull/commit/delete flows over those primitives) ·
`jobs` (the clone/delete/pull/commit Operation machine) · `binswap` (automatic clone-binary
hot-swap: hash-check on daemon `Hello` + a periodic sweep) · `linear` · `claude` (usage poll
+ token refresh/push + assign/swap) · `chat` (agent-wrapper proxy + per-host SSE) · `monitor`
(host poller) · `homes` (clone-home symlinks under `data/hosts/`) · `smb` (smbd supervisor +
read-write `clones` share over `data/hosts`) · `files`
(notes/uploads/detector-feedback) · `assets` (on-disk clone-daemon/agent-wrapper payloads + the
served frontend).

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

State store + SSE, all `/api/*` routes, the served SPA, and `/uploads`. Orchestration
(clone/delete/pull/commit + images over the local Docker daemon, Linear, Claude, chat proxy,
monitor poller, clone-home reconciler). Every endpoint is documented in
[API.md](../../docs/API.md). Config is edited via the Settings UI: `GET /api/config` returns a
redacted view, `PUT` merges + persists 0600 + applies live, `POST /api/config/test {docker}`
checks the Docker environment (mirrored row-by-row at `GET /api/setup/env`).

## Ports 3 & 4 — MCP (`mcp`)

Hand-rolled JSON-RPC-over-HTTP (curl-testable; not `rmcp`).
- **Port 3 (per-clone, header-routed):** the one tool is `set_state` — the in-clone agent reports
  `working`/`idle` + a note; the clone self-identifies via the `x-rmng-clone` header.
- **Port 4 (fleet):** web-action tools (`list_hosts`, `select`, `clone`, `delete`, `claude_*`,
  `set_state`, `send_message`, `read_chat`) run locally; desktop/window tools (`screenshot`,
  `mouse_move`, clicks, `scroll`, `key`, `type`, `list_windows`, `move_window`, `list_apps`,
  `launch_app`) are **proxied** to the addressed clone's daemon MCP at
  `http://{host}:{daemon_mcp}`. There is no `redeploy` tool — clone binaries hot-swap
  themselves (see below).

The full desktop-automation surface lives in the **clone-daemon** (`:9004`), not here — the
in-clone agent calls it directly on localhost and the fleet MCP proxies to it. Every tool +
args: [MCP.md](../../docs/MCP.md).

## Claude account assignment & swap (`claude`)

Each account has **two credentials**: a **refresh token** (+ cached short-lived access token)
used server-side **only** to read 5h/7d usage (429 backoff), never sent to a clone; and a
**long-lived token** that actually runs Claude Code. Delivery writes the clone's
`~/.claude/.credentials.json` (long-lived token, refresh **emptied** so the SDK never rotates
it) — read at request time, so a **running** clone hot-swaps with no restart. **Auto-assign**
at clone time by usage+load score; **hot-swap** from the UI/`/api/claude/swap`/fleet MCP.

## Orchestration (`docker`, `provision`, `jobs`)

`docker` holds the bollard client + dumb primitives (create/start/stop/commit/exec/tar/network);
`provision` stitches them into clone-create, template-pull, commit-from-clone, and delete
flows, streaming progress through a `FnMut(&str, &str)` callback (the old `P step msg` /
`RESULT` bash protocol is gone); `jobs` wraps each in an `Operation` streamed over `/events`.
Clone sources are **images** (`rmng.image=1`, identified by their own `repo:tag` such as
`pegasis0/rmng-template:latest`) — no golden-CT / CoW model: the template is built + published
ahead of time (`template/Dockerfile`, not by this crate — see
[DEPLOY.md#publishing-the-template](../../docs/DEPLOY.md#publishing-the-template)),
`pull_template` pulls it (no local retag — it keeps its own `repo:tag`), clones are `docker run`
off an image, and any clone commits to a new image. In-container guest scripts (`claude-import.sh`)
run over `docker exec bash -s`. See [DEPLOY.md](../../docs/DEPLOY.md) and
[SCRIPTS.md](../../docs/SCRIPTS.md).

## Automatic clone-binary hot-swap (`binswap`)

Replaces the old manual per-host "redeploy": at [`main`](src/main.rs) startup `binswap::spawn`
hashes the `clone-daemon`/`agent-wrapper` payloads this image ships (once), then a single
worker task hashes each running clone's on-disk `/opt/rmng/bin/*` and bounces just the
`systemd --user` unit(s) whose hash is stale — via [`provision::redeploy_clone`](src/provision.rs),
which pushes the binary via tar and does the `systemctl --user stop/start` dance. Two
enqueue paths: `mediaplane` calls `SwapState::request_check` on a clone's `Hello`, and a sweep
loop enqueues every managed container 60 s after boot, then every 5 min. Failed swaps back off
(`30s · 2^failures`, capped at 30 min); a payload that changed on disk *after* the hashes were
warmed is refused with a WARN rather than risked into a swap loop. Details:
[DEPLOY.md#upgrades](../../docs/DEPLOY.md#upgrades).

## Networking

Only the control-server needs external reachability (tailscale, manual). Clones sit on the
user-defined `rmng` Docker bridge (static IPs: `.1` gateway, `.2` control-server, `.10+`
clones), reachable *from* the control-server (the agent-wrapper chat proxy + the
fleet-MCP→daemon-MCP proxy); media/input cross the shared `/srv/rmng-sock` named-volume unix
socket (SCM_RIGHTS), not the network. Exposure split: ports 1+2 operator-facing; port 3
internal bridge only (header-routed via `x-rmng-clone`); port 4 most-privileged
(localhost/token); the forward data plane (9005) and the `clones` SMB share (445) are both
published for the viewer / clone-home browsing.

## Dependencies

`axum`/`tokio`/`tower-http` (port 2 + the MCP HTTP servers + static files), `reqwest` (Linear,
Claude, agent-wrapper, the daemon-MCP proxy — plain HTTP, no rustls/native-tls), `bollard` +
`tar` (Docker orchestration over the unix socket), `notify` (file watch), `serde_json`,
`wire`, `media`.

## Tests

`cargo test -p control-server` (run where GStreamer links — the crate pulls in `media`): the
subnet/IP allocator + image-reference canonicalization + step→percentage tables (`provision`/`docker`),
account scoring, config defaults/merge/redaction + one-time/restart-required categories,
source-IP→clone mapping, `in_use_by` accounting, and `binswap`'s `sha256sum`-output parsing +
swap backoff progression.
