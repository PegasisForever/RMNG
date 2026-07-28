# control-server

The backend binary — one tokio service that is the **control plane**, the **media plane**,
and the **fleet-automation plane**. It exposes three service ports (9000 web, 9001 video,
9005 forward) plus the clone-home SMB share on 445, and ships as a Docker image; the frontend
and the `clone-daemon`/`agent-wrapper`/`rmng-cli` binaries are plain on-disk payloads under
`/usr/local/share/rmng/` (read at runtime, injected into clones at create time) — nothing is
compiled into the binary. Clones themselves are created from a separately-published **template**
image (`pegasis0/rmng-template`, built by `template/Dockerfile`), pulled by `POST
/api/images/pull` — not built in-product, so the patched gnome-shell `.deb` isn't a
control-server payload at all. Full references: [API](../../docs/API.md) ·
[MCP](../../docs/MCP.md) · [PROTOCOL](../../docs/PROTOCOL.md) · [DEPLOY](../../docs/DEPLOY.md).

| Port | Default | Transport | Serves |
|---|---|---|---|
| **1 — video** | 9001 | framed H.264/JSON over TCP | the native [viewer](../viewer/README.md): selected clone's monitors out; input/clipboard/cursor |
| **2 — web API** | 9000 | `axum` HTTP + SSE + embedded frontend | the [frontend](../../frontend/README.md): `/events`, all `/api/*`, the SPA |
| **4 — forward** | 9005 | framed TCP over TCP | the viewer's port-forward data plane: one TCP connection per accepted local socket, spliced to the clone |
| **SMB** | 445 | SMB (smbd) | the `clones` share (fixed cred `rmng`/`rmng`) browses each running clone's `/home/rmng` at `smb://<host>/clones` |

**No clone model traffic passes through this process.** Agents dial their providers directly and
authenticate from credential files this server writes into each clone (see
[Accounts](#accounts-claude--codex)) — which is what makes updating or restarting the
control-server safe while clones are mid-turn.

## Modules

`app` (shared state holder) · `state` (in-memory `ControlState` + atomic `state.json` persist
+ file-watch + SSE bus) · `config` (load/merge/redact `config.json` at 0600) · `web` (port 2
routes + SSE + SPA + the desktop/exec proxy endpoints for `rmng desktop`/`rmng exec`) ·
`clonekey` (per-clone identity bearers, `RMNG_PROXY_KEY`) · `mediaplane` (port 1: clone-socket
ingest → `media` encode → viewer; input routing;
clipboard broker) · `forward` (port-forward data plane: viewer TCP spliced to the clone) ·
`docker` (bollard primitives against the local daemon) · `provision` (clone/pull/commit/delete
flows over those primitives) · `clone_reconcile` (the 30 s live-migration pass over running
clones: payloads, sshd, `/etc/environment`, the generated agent configs) · `jobs` (the
clone/delete/pull/commit Operation machine) · `linear`
· `claude` / `codex` (the two account stores: usage poll + OAuth refresh + token push +
assign/swap/rotate) · `clone_ops` (what those two share: the guest-script exec path, JWT decode,
provider-scoped view replacement) · `token_unmigrate` (the one-shot startup migration off the
retired routed model) · `chat` (agent-wrapper proxy +
per-clone SSE + the activity stream behind working/idle) · `monitor` (Docker maintenance,
CPU/RAM sampling, the activity bus and lifecycle writer) · `homes`
(clone-home symlinks under `data/hosts/`) · `smb` (smbd supervisor + read-write `clones` share
over `data/hosts`) · `files` (notes/uploads) · `assets` (on-disk clone-daemon/agent-wrapper
payloads + the served frontend).

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

Operator/fleet **desktop control** uses the `rmng desktop` CLI
([crates/cli](../cli/README.md)), which posts to the web port's
`POST /api/hosts/:id/mcp` endpoint; the server forwards each call verbatim to the addressed
clone's daemon MCP at `http://{host}:{daemon_mcp}`. Fleet management (clones, clone/delete,
images, accounts) and `rmng exec` go through the same `rmng` CLI over the port-2 web API.

The full desktop-automation surface lives in the **clone-daemon** (`:9004`), not here — the
in-clone agent calls it directly on localhost and the `rmng desktop` CLI proxies to it via the
web API. Every tool + args: [MCP.md](../../docs/MCP.md).

<a id="accounts-claude--codex"></a>
## Accounts (`claude` / `codex`)

**Single-token model, server-owned.** An account is just its OAuth pair (access + single-use
refresh token) in a 0600 store — `claude-accounts.json` / `codex-accounts.json` — and this
process owns the entire refresh lifecycle: nothing that can refresh ever leaves it. A clone is
authed by writing **only the current access token** into `~/.claude/.credentials.json` /
`~/.codex/auth.json`, with an **empty refresh token** and a far-future expiry, so the clone's CLI
can neither rotate (and thereby invalidate) the server's token nor decide it has expired. Every
refresh that rotates a token fans it back out to that account's clones (`push_stale_tokens`).
Both files are read per request → a **running** clone hot-swaps with no restart.

**Importing** harvests the pair from a clone already signed in via claude.ai / ChatGPT, then
deletes the clone's own credential file. **Selection** is the operator's intent verbatim — an
email, `auto`, `none`, or `group:<pool>` — kept on the clone alongside the resolved account, so
an auto-managed clone stays distinguishable from a pinned one. Pools live in `config.json`
(`cloneGroups`/`codexGroups`) and are edited wholesale through `PUT /api/config`; a 10-min sticky
rotator moves a clone only when its account falls out of eligibility, since a switch cold-starts
the clone's prompt cache. The two modules are near-symmetric; what they share
(`docker exec` guest-script runner, JWT decode, provider-scoped view replacement into
`ControlState.claudeAccounts`) lives in `clone_ops`.

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
off an image, and any clone commits to a new image. In-container guest scripts
(`claude-import.sh`, `codex-import.sh` — one per provider, same `status`/`read`/`apply`/`clear`
verbs) run over `docker exec bash -s`. See [DEPLOY.md](../../docs/DEPLOY.md) and
[SCRIPTS.md](../../docs/SCRIPTS.md).

## Clone binaries — create-time injection

The server installs its own current payloads into every clone **before it boots**
([`provision.rs`](src/provision.rs) `CLONE_BINARIES`): `clone-daemon` + `agent-wrapper` to
`/opt/rmng/bin` (the `systemd --user` units exec them by absolute path) and the `rmng` fleet
CLI to `/usr/local/bin/rmng` (on every shell's PATH). This is the **sole delivery path** —
the template carries none of them, a fresh clone always runs binaries matching the server
that created it, and existing clones keep theirs across a server upgrade (the binswap
hot-swap engine is retired). Details: [DEPLOY.md#upgrades](../../docs/DEPLOY.md#upgrades).

## Networking

Only the control-server needs external reachability (tailscale, manual). Clones sit on the
user-defined `rmng` Docker bridge (static IPs: `.1` gateway, `.2` control-server, `.10+`
clones), reachable *from* the control-server (the agent-wrapper chat proxy + the
`rmng desktop` → daemon-MCP proxy + `rmng exec`); media/input cross the shared
`/srv/rmng-sock` named-volume unix socket (SCM_RIGHTS), not the network. Ports 1 and 2 are
operator-facing; the clone daemon MCP (9004) is localhost/token-protected; the forward data
plane (9005) and the `clones` SMB share (445) are published for the viewer and clone-home
browsing.

## Dependencies

`axum`/`tokio`/`tower-http` (port 2 + static files), `reqwest` (the Anthropic + OpenAI OAuth/usage
endpoints, Linear, agent-wrapper, the daemon-MCP proxy — `rustls-tls`, since the provider calls
are HTTPS), `bollard` +
`tar` (Docker orchestration over the unix socket), `notify` (file watch), `serde_json`,
`wire`, `media`.

## Tests

`cargo test -p control-server` (run where GStreamer links — the crate pulls in `media`): the
subnet/IP allocator + image-reference canonicalization + step→percentage tables (`provision`/`docker`),
account scoring / assignment / rotation for both providers, the injected credential-file shapes
(access token in, refresh token emptied), provider usage-window parsing, the reverse token
migration (parse, dedupe, 0600 stores), per-clone identity keys, the `/etc/environment` sync's
retired-key stripping, config defaults/merge/redaction + one-time/restart-required categories,
Docker lifecycle transitions, and `in_use_by` accounting.
