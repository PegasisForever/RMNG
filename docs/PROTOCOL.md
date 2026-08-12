# Protocol, config & internals reference

Everything below the HTTP/MCP layer: the port-1 viewer wire protocol, the clone↔server
unix socket, the config schema, every environment variable, the clone-daemon CLI, and each
crate's public Rust API. Sources: [crates/wire/src/socket.rs](../crates/wire/src/socket.rs),
[viewer.rs](../crates/wire/src/viewer.rs), [config.rs](../crates/wire/src/config.rs),
[control-server/src/mediaplane.rs](../crates/control-server/src/mediaplane.rs),
[clone-daemon/src/transport.rs](../crates/clone-daemon/src/transport.rs),
[media/src/sock.rs](../crates/media/src/sock.rs).

## Ports & sockets

| Name | Default | Override | Listener | Connected by | Transport |
|---|---|---|---|---|---|
| video | `9001` | `listen.video` | control-server mediaplane | native viewer | framed H.264/JSON over TCP |
| web | `9000` | `listen.web` | control-server web | browser / `rmng` CLI / control-client | HTTP + SSE |
| forward | `9005` | `listen.forward` | control-server mediaplane | native viewer | framed TCP over TCP (one conn per forwarded local socket, spliced to the clone) |
| bastion | `2222` | `listen.bastion` | control-server ssh.rs | operator ssh client (jump) | OpenSSH; pubkey-only; forwards to `clone:22` |
| daemon MCP | `9004` | `RMNG_DAEMON_MCP_PORT` | clone-daemon | agent-wrapper + `rmng desktop` proxy (via web API) | HTTP JSON-RPC |
| agent-wrapper | `4096` | `agent_port` (config) / `AGENT_PORT` | agent-wrapper (in clone) | control-server chat proxy | HTTP + SSE |
| clone socket | `/srv/rmng-sock/clones.sock` | `cloneSocket` config (server) / `RMNG_SOCKET` (daemon) | control-server mediaplane | clone-daemon | unix `SOCK_SEQPACKET` + `SCM_RIGHTS` |
| holder socket | `$XDG_RUNTIME_DIR/rmng-session-holder.sock` | `RMNG_HOLDER_SOCKET` | session holder (in clone) | clone-daemon | unix `SOCK_SEQPACKET` |

---

## Port-1 viewer protocol (viewer ⇄ control-server)

One TCP connection. Every frame is `[u8 tag][…]`. This is the **verified on-wire framing**
(the `ToViewer`/`FromViewer` enums in `wire/viewer.rs` are the logical model; the live media
path uses this compact framing carrying `socket.rs` types).

**Server → viewer:**

| Tag | Name | Frame |
|---|---|---|
| `0` | video | `[0][u32be monitor_id][u32be len][AnnexB access-unit]` |
| `1` | clipboard | `[1][u32be len][JSON ClipboardMsg]` |
| `2` | cursor | `[2][u32be len][JSON CursorMeta]` |
| `3` | layout | `[3][u32be len][JSON MonitorPlacement[]]` |

**Viewer → server:** `[u8 tag][u32be len][JSON body]`, body cap 1 MiB.

| Tag | Body | Meaning |
|---|---|---|
| `0` | `InputMsg` | an input event for the **selected** clone (note: upstream tag 0 carries input, not video) |
| `1` | `ClipboardMsg` | the viewer's clipboard offer/request/data (brokered to clones) |

`InputMsg` ([socket.rs](../crates/wire/src/socket.rs), serde tag `kind`, snake_case):

| Variant | Fields | Use |
|---|---|---|
| `pointer_move` | `monitor_id`, `x`, `y` (f64) | absolute pointer in monitor-pixel space (**native**, see below) |
| `pointer_relative` | `dx`, `dy` (f64) | unaccelerated delta — pointer-lock / games |
| `button` | `button` (evdev: `0x110`–`0x112` left/right/middle, `0x113`/`0x114` back/forward), `pressed` | mouse button |
| `axis` | `axis` (0=vert,1=horiz), `step` (±1) | discrete scroll |
| `key` | `keysym` (X11), `pressed` | text/modifier key (MCP `key` path) |
| `key_code` | `keycode` (evdev = GTK `hardware_keycode − 8`), `pressed` | physical-key identity (games) |

The viewer sends `pointer_move`/`button`/`axis`/`key` from normal GTK input; `key_code` for
physical keys; `pointer_relative` while pointer-lock is engaged (Ctrl+Alt+G). When the server
sends a `CursorMeta{warp:true}` (an MCP-driven move) the viewer snaps the drawn remote cursor
and **suppresses local `pointer_move`/`pointer_relative` for ~0.5 s** so the operator's mouse
doesn't fight the agent.

> **This path is native and stays native.** Coordinates here are the monitor's real pixels, and
> the H.264 stream is encoded at full resolution. The 1080p virtual space is a property of the
> **daemon MCP tool channel only** ([MCP.md](MCP.md)) — the daemon converts virtual → native
> before it ever reaches this socket. Don't "fix" the viewer or the encoder to 1080p to match.

---

## Clone socket protocol (clone-daemon ⇄ control-server)

A unix `SOCK_SEQPACKET` socket (one JSON message per datagram). dmabuf file descriptors ride
out-of-band via `SCM_RIGHTS` in the same datagram, in plane order — never in the JSON. The
daemon connects to `RMNG_SOCKET`; the server listens on the `cloneSocket` config path
(default `/srv/rmng-sock/clones.sock`, one-time — set in the setup wizard, baked into every
clone's `RMNG_SOCKET` at bootstrap; a pre-latch edit is restart-required). The socket lives in
the shared **`rmng-sock` named volume**, mounted at the same path `/srv/rmng-sock` into the
control-server **and** every clone (a named volume, not a bind, so siblings can share it);
`chmod 0777` so cross-uid clones connect.

**Handshake:** the daemon's first message is `DaemonMsg::Hello { clone_id, fresh_session }`.
`fresh_session` is true when this daemon had to start the session holder, so the desktop
behind it is seconds old and holds no window anyone placed. The server answers those with the
active layout preset even when the clone is not the selected one, because a holder that
remembered nothing comes up on the built-in single 1920x1080. The field defaults to false, so
a daemon older than it reads as "the holder was already running" and is left alone.

`DaemonMsg` (daemon → server), serde tag `t`:

| Variant | Payload | Meaning |
|---|---|---|
| `hello` | `{clone_id, fresh_session}` | register the clone; `fresh_session` asks for the active layout |
| `frame` | `FrameMsg` | one captured monitor frame; dmabuf fds attached via SCM_RIGHTS |
| `cursor` | `CursorMeta` | cursor position (+shape on change, +`warp` if MCP-driven) |
| `layout` | `{monitors: MonitorPlacement[]}` | the actual applied monitor layout |
| `clipboard_offer` / `clipboard_request` / `clipboard_data` | resp. types | clipboard bridge |

`ServerMsg` (server → daemon), serde tag `t`: `subscribe {stream:bool}` (start/stop the
continuous feed), `frame_request {monitor_id}` (one-shot, screenshot path), `ack
{monitor_id, seq}` (flow control — the daemon waits for the ack before the next frame),
`input(InputMsg)`, the three `clipboard_*` messages, and `set_monitors {monitors:
MonitorSpec[]}` — apply a layout **live**: the daemon does a make-before-break session swap
(rebuilds a fresh Mutter session with the desired monitors, switches capture + input to it,
then stops the old one, so running apps never close). Sent to the selected clone: on its
`Hello`, on `POST /api/layout/activate`, on a `PUT /api/config` that moves the active preset's
geometry, and on the `POST /api/activate` that brings it on screen. Sent to any clone, watched
or not, on a `Hello` that carries `fresh_session`, and once to a brand-new clone when its
daemon first registers. Every other clone keeps the layout it was last viewed with.

`FrameMsg`: `monitor_id`, `fourcc` (DRM, e.g. `0x34325241` "AR24"), `modifier` (DRM format
modifier), `width`, `height`, `planes: [{offset, stride}]`, `seq` (echoed in `ack`).

### CursorMeta
```rust
struct CursorMeta {
    monitor_id: u32, x: i32, y: i32,
    shape: Option<CursorShape>,   // only on shape change; positions carry None
    warp: bool,                   // #[serde(default)] — true = server/MCP-initiated warp
}
struct CursorShape { width, height, hotspot_x, hotspot_y, rgba: Vec<u8> /* base64 in JSON */ }
```
Captured out-of-band as `SPA_META_Cursor` (cursor-mode METADATA, via the raw-PipeWire path
since GStreamer `pipewiresrc` can't surface it) and drawn client-side. `warp:true` triggers
the viewer's 0.5 s local-motion suppression.

### Clipboard (rich + lazy)
`ClipboardOffer {serial, mime_types[]}` advertises types (no bytes). `ClipboardRequest
{serial, mime_type}` asks for one. `ClipboardData {serial, mime_type, bytes /* b64 */}`
transfers. `ClipboardMsg` (serde tag `k`: `offer`/`request`/`data`) is the port-1 viewer-side
envelope. The control-server is the **broker**: it tracks the owner, fans each offer to the
viewer + every other clone, routes a paste's request to the owner, and routes bytes back to
the requester. The clone-daemon bridges via Mutter `RemoteDesktop` selection
(`SelectionRead`/`SelectionWrite`); the viewer via the GTK clipboard.

**An image does not fit in one message, on either hop.** Both had a ceiling sized for text,
and a pasted screenshot crossed neither.

The clone socket is `SOCK_SEQPACKET`, and the kernel refuses a datagram over `SO_SNDBUF - 32`,
about 208 KB at the default `net.core.wmem_default`. A 256 KB payload is 349,596 bytes of JSON,
so `sendmsg` returned `EMSGSIZE` and the broker discarded the error. A message past the ceiling
is now split into 64 KiB chunks framed `\0RMC | id | index | count | slice` and rejoined by the
receiver. The magic starts with a NUL, which no JSON document can, so a whole message is still
sent exactly as it was and needs no envelope. The receive queue holds about three chunks, so a
chunked send waits for the peer to drain between datagrams, bounded at 10 s.

The viewer connection framed everything with a 1 MiB cap, and passing it did not skip the frame,
it dropped the viewer: that reader owns the viewer's teardown. Pasting a 1.5 MB image sent a
2,000,065-byte frame and killed the whole input channel. Clipboard frames now allow 32 MiB and
every other tag keeps the old 1 MiB, because an input event and a status line have no business
being large.

**A paste that gets no bytes fails after 10 s** rather than hanging. Mutter opens the pipe and
the pasting application blocks on it until somebody writes and calls `SelectionWriteDone`, so a
reply that never arrives is not a dropped paste, it is a frozen one, and what the application
eventually shows is whatever it had. That is what made a too-large image read as slow and then
corrupt instead of as an error.

**One read at a time.** Mutter runs a single `SelectionRead` per session and fails a second
one with `LimitsExceeded: Tried to read in parallel`, which reaches the requester as empty
bytes. One copy fans out to every endpoint, and each asks per MIME, so two rules keep the
reads sequential. The broker forwards only the first request for a `(serial, mime_type)` and
adds later requesters to the same pending list, and the clone-daemon holds a gate across the
whole transfer, from `SelectionRead` to the end of the fd read.

---

## Holder socket protocol (clone-daemon ⇄ session holder)

Two processes inside each headed clone. `rmng-session-holder.service` owns the Mutter
RemoteDesktop and ScreenCast sessions, the virtual monitors, `ApplyMonitorsConfig`, input
injection and the clipboard bridge. `rmng-clone-daemon.service` owns capture, encode,
shipping, and the MCP on `:9004`. Both are the same binary, the holder started with
`--session-holder`. Source: [holder.rs](../crates/wire/src/holder.rs).

**Why they are split.** Mutter destroys a RemoteDesktop session when the D-Bus connection
that created it drops, and gnome-shell remaps every window when the monitor set empties. The
daemon restarts on every payload push, so a daemon that owned the session reset every window
position on each update: measured on a 1920x1080 clone, a window at (100,100) came back at
(63,57), and on a two-monitor layout every floating window collapsed onto monitor 0. The
holder does not restart on a payload push, so the monitors never go away and nothing moves.

**Why input and the clipboard cross the socket.** Mutter answers session methods only for the
connection that created the session: the same call from another connection returns
`org.freedesktop.DBus.Error.AccessDenied`. `org.gnome.Shell.Eval` has no such check, so the
window tools (`list_windows`, `move_window`) stay in the daemon on its own bus connection.
Capture stays there too: a PipeWire node is reachable by id from any client on the clone's
socket, verified against a node another process created.

**Messages**, one JSON object per datagram, chunked over 64 KiB exactly as the clone socket
is. No file descriptors cross.

| Direction | Message | Meaning |
|---|---|---|
| daemon → holder | `hello{proto}` | First message. Answered by `hello_ok`. |
| daemon → holder | `input` | One `InputMsg` to inject. Fire-and-forget, ordered. |
| daemon → holder | `set_layout{monitors}` | Apply a layout, make-before-break. No-op if unchanged. |
| daemon → holder | `capture_ready{generation}` | Capture is live on that generation's nodes. |
| daemon → holder | `clipboard_offer` / `clipboard_request` / `clipboard_data` | Relayed from the broker. |
| holder → daemon | `hello_ok{proto, generation, monitors}` | The monitor set already held. |
| holder → daemon | `monitors{generation, monitors}` | A new set exists and is capturable. |
| holder → daemon | `swap_done{generation}` | The old monitors are gone; drop their capture. |
| holder → daemon | `clipboard_offer` / `clipboard_request` / `clipboard_data` | Relayed to the broker. |

**A layout swap is a three-step handshake**, so no frame gap opens in the middle of it. The
holder builds the new session alongside the old and sends `monitors`. The daemon starts
capture on the new nodes and answers `capture_ready`. The holder stops the old session, waits
for its connectors to actually disappear, applies the layout, and sends `swap_done`, at which
point the daemon drops the old captures. Waiting for the connectors is not optional: applying
a config against dying ones is silently ignored by Mutter at best and crashed gnome-shell
fleet-wide once.

**Version skew.** The daemon compares `PROTO_VERSION` with the holder's and restarts the
holder exactly once on a mismatch, which costs one window reset on that release and nothing
on the others. Nothing else restarts the holder: the reconcile pass starts and enables it,
never restarts it.

**The layout the holder boots on** comes from `~/.rmng/monitors`, written after every applied
layout, and falls back to `RMNG_MONITORS` on a clone that has never run one. A value baked
into a clone image goes stale the first time the operator switches presets, and booting on it
costs a session build and a swap that the control-server's push then has to undo.

---

## Config schema

`AppConfig` loads from `./config.json` in the working directory (no env override — the Docker
image sets `WORKDIR /data`, so `config.json` + `data/` land in the `rmng-data` volume);
written at `0600`. The web API returns `AppConfigRedacted`, which carries the preset Linear
keys verbatim as `linearKey: string` (the clients are what call Linear) and withholds nothing
else: every other credential the server holds lives in its own account store rather than in
the config. `PUT /api/config` returns
`{ config: AppConfigRedacted, restartRequired: bool, networkWarning?: string }`. Source:
[config.rs](../crates/wire/src/config.rs).

| Field | Type | Default | Notes |
|---|---|---|---|
| `listen` | `ListenConfig` | see below | the web, video, daemon MCP, forward, and bastion ports |
| `agent_port` | u16 | `4096` | agent-wrapper port on each clone |
| `data_dir` | string | `"data"` | state, notes, uploads, chats, and private clone-token totals root; `state.json` and the `claude-accounts.json` secret store live here. **One-time** (set in the setup wizard) |
| `static_dir` | string | `""` (embedded) | empty serves the frontend embedded in the binary; a non-empty disk path serves the bundle from there. Set in Settings → Advanced. **Restart-required** |
| `clone_socket` | string | `/srv/rmng-sock/clones.sock` | media-plane unix socket the clone-daemons connect to; baked into the template at provision. Set in the setup wizard. **One-time** (a pre-latch edit is **restart-required** — the old path is bound at startup) |
| `chroma` | `ChromaMode` | `4:2:0` | viewer video chroma subsampling. Settings → Video. **Restart-required** |
| `setup_complete` | bool | `false` | latched `true` by the first-run setup wizard; gates the frontend to the wizard until then |
| `layout_presets` | `LayoutPreset[]` | `[]` | named monitor-layout presets (`{name, monitors: MonitorSpec[]}`); the operator switches the active one from the sidebar (`POST /api/layout/activate`) |
| `active_layout` | string | `""` | name of the active layout preset; drives `effective_monitors()` (see below) |
| `docker` | `DockerConfig` | see below | daemon socket + `rmng`-network subnet + hostname prefix + per-clone limits |
| `presets` | `Preset[]` | `[]` | clone presets: env vars + Linear key + auto-select ticket-id prefixes (the key is a credential, and `GET /api/config` vends it to the clients) |
| `claude` | `ClaudeConfig` | — | usage polling config |
| `clone_groups` | `CloneGroup[]` | `[]` | named account pools for Claude rotation (not secret) |
| `codex` | `CodexConfig` | — | Codex usage polling config |
| `codex_groups` | `CloneGroup[]` | `[]` | named account pools for Codex rotation (not secret) |
| `agent_playbook` | string | shipped default | the desktop agent's base playbook (operating notes + ticket procedure), injected into each new clone at creation as its system-prompt append (written to the clone's `~/.config/rmng/agent-instructions.md`, where the agent-wrapper reads it, overriding its baked-in fallback). Seeded from the wrapper's `agent-instructions.md`; editable in Settings; **non-secret** (passes through the redacted view); applies to the next clone (**not restart-required**) |
| `judge` | `JudgeConfig` | `gpt-5.6-luna` | working-vs-stuck detection: `codexModel` (default `gpt-5.6-luna`) and `codexEmail` (`null` = the first imported Codex account). No credential of its own: the calls run on that account's ChatGPT plan, using the token the server already holds to run its clones. Nothing secret, so the whole struct passes through the redacted view. No Codex account imported ⇒ no clone is ever reported `working` (see [monitorState](API.md#monitorstate)). Settings → Agents; **not restart-required** |

- **`ListenConfig`**: `web 9000`, `video 9001`, `daemon_mcp 9004`, `forward 9005`, and `bastion 2222`.
- **`DockerConfig`** (no secret — the local daemon is reached over a unix socket, so the
  whole struct passes through the redacted view): `socket`
  (`"/var/run/docker.sock"` — the daemon the control-server drives, **restart-required**;
  the bollard client is built at startup), `subnet` (`"10.99.0.0/24"` — the CIDR for the
  user-defined `rmng` bridge; addressing is Docker DNS — clones resolve by container name,
  the control-server by its `rmng-control` alias — with clone IPs left to Docker IPAM;
  validated `/16`–`/24` at merge; **one-time**, baked into the network at first-run setup), `hostname_prefix` (`"pega-"`, editable in Settings → prepended to derived
  clone hostnames; carried from the retired `proxmox.hostname_prefix` on migration),
  `clone_cpus` (`16` — whole cores → `nano_cpus`) and `clone_memory_mb` (`32768` — MiB, +8 GiB
  swap), both editable per-clone limits, and `template_reference`
  (`"pegasis0/rmng-template:latest"` — the registry `repo:tag` the wizard/API pulls the clone
  template from at `POST /api/images/pull`; editable, no secret).
- **First-run setup wizard**: a fresh deploy ships `config.json` with `"setupComplete":
  false`, so the web UI shows the wizard (environment checklist → server settings + monitors
  → download the clone template → finish) instead of the dashboard; finishing latches
  `setupComplete: true` (a one-way latch) and materializes the lazy `rmng` network, after
  which the one-time fields (`data_dir`, `clone_socket`, `docker.subnet`) are locked. There is
  **no grandfather rule**: an old `config.json` re-runs the wizard (new machine, no network /
  template pulled yet). A legacy `proxmox` block is scrubbed on load, carrying `hostnamePrefix` into
  `docker.hostname_prefix`; old `state.json` clones load as plain unmanaged rows
  (`managed: false`; serde drops the stale `ctid`/`container` keys).
- <a id="preset"></a>**`Preset`**: `name`, `labels` (ticket-id prefixes / Linear team keys,
  e.g. `DEV`, that auto-select this preset when cloning from a ticket — matched
  case-insensitively against the ticket's prefix like `DEV-196` → `dev`, first match in
  config order wins), `linear_key` (personal API key — injected into the clone as
  `LINEAR_API_KEY` authing its `linear` MCP, and handed to the clients verbatim by
  `GET /api/config`, which is what the ticket column lists issues with and what the clone
  dialog and the `rmng` ticket verbs resolve a ticket with), `vars`
  (env vars written to the clone's `/etc/environment`), and `agent_playbook` (optional,
  non-secret — text appended after a blank line to the global `agent_playbook` for
  clones of this preset; empty ⇒ global only). `PUT /api/config` merges rows by name
  (blank `linearKey` keeps the stored one; omitted row deletes). One-shot migration at
  load: legacy `envPresets` seed `presets` (no labels/keys); legacy per-workspace `linear`
  keys are dropped (re-enter per preset in Settings).
- **`ClaudeConfig`**: `poll_secs` (`600`, floored 15), `pinned_email?`.
- <a id="claude-accounts"></a>**Claude accounts** live outside config, in the server's 0600
  secret store `claude-accounts.json`: per account an OAuth pair (`access_token` +
  single-use `refresh_token`, both **secret**), obtained by signing in to the provider at
  the server (`POST /api/login/begin` then `/api/login/complete`).
  The server owns the whole refresh lifecycle; a clone gets **only the current short-lived
  access token** written into its `~/.claude/.credentials.json` (refresh emptied, far-future
  expiry), re-pushed to every assigned clone whenever a refresh rotates it — so a *running*
  clone hot-swaps without restart (written via `docker exec` into the clone).
- **`CloneGroup`**: `name`, `accounts` (member emails). A clone bound to a group
  (`Clone.claude_group`) sticks to its account (preserving its prompt cache) until that
  account is exhausted (80% 5h or 95% 7d) or leaves the group; the 10-min rotator then
  moves it to the least-loaded / least-used member. Selected at clone/swap time as
  `group:<name>`. A clone selected as `auto` rotates across all imported accounts using
  this same sticky rotation.

<a id="codex-accounts"></a>**Codex accounts** — server-owned single-token model, identical in spirit to Claude accounts.

- **Store:** `codex-accounts.json` (0600, in `data_dir`; override `RMNG_CODEX_ACCOUNTS_FILE`).
  Each record: `id` (`codex:<account_id>`), `email`, `account_id`, `plan`, `access_token`,
  `id_token`, `refresh_token`, `expires_at`.
- **Injected in-clone file:** `~/.codex/auth.json` = `{ "OPENAI_API_KEY": null, "tokens":
  { "id_token", "access_token", "refresh_token": "", "account_id" }, "last_refresh": <now> }`.
  The refresh token is emptied and `last_refresh` set to now so the clone's CLI never
  rotates the server-owned token. The server re-pushes on every refresh, with a 60-min lead.
- **Refresh:** `POST https://auth.openai.com/oauth/token` (client_id
  `app_EMoamEEZ73f0CkXaXp7hrann`). No `expires_in` — expiry is decoded from the access-token
  JWT `exp`. Refresh tokens are single-use / rotating.
- **Usage:** `GET https://chatgpt.com/backend-api/wham/usage` (Bearer + `ChatGPT-Account-Id`);
  windows map to 5h/weekly by `limit_window_seconds`. Disable with `codex.usagePolling=false`
  (refresh + push still run).
- **`CodexConfig`**: `poll_secs`, `pinned_email?`, `usage_polling` (bool, default `true`).
- **`codexGroups`** (`CloneGroup[]`): same structure as `clone_groups`, used for Codex
  account rotation. Selected at clone/swap time as `group:<name>`.
- **`Clone`** carries `codexAccountEmail` / `codexGroup` / `codexSelection` alongside the
  Claude equivalents. One clone can hold both a Claude and a Codex account simultaneously.

- **`MonitorSpec`**: `width`, `height`, `x`, `y`, `primary`.
- **`LayoutPreset`**: `name`, `monitors: MonitorSpec[]` — a full named arrangement the
  operator can switch to live. `AppConfig::effective_monitors()` returns the active preset's
  (`active_layout`) monitors, falling back to the first preset, then a built-in dual
  2560×1440 default when `layout_presets` is empty.

Template params are mostly not config: the base OS is fixed in the template build
(`ubuntu:26.04` in `template/Dockerfile` — the patched gnome-shell is compiled against 26.04's
GNOME only) and isn't chosen at pull time. The wizard/API pull takes an optional registry
reference (`POST /api/images/pull {reference?}`; the pulled image keeps its own `repo:tag` as
the clone-source reference, no retag; `reference` defaults to `docker.template_reference`).
Per-clone CPU / memory limits come from
`docker.clone_cpus` / `docker.clone_memory_mb`, applied at clone create — not per image.

---

## Environment variables

**control-server:** reads **no `RMNG_*` env vars** — all config is `./config.json` in the
working directory (the Docker image sets `WORKDIR /data`, the `rmng-data` volume). The
disk-frontend path, chroma, and Docker daemon socket are the `staticDir` / `chroma` /
`docker.socket` config fields (restart-required, along with the five listen ports); the clone
socket is the `cloneSocket` config field (**one-time** — baked into every clone's
`/srv/rmng-sock` bind + clone-daemon `RMNG_SOCKET` at bootstrap — but a pre-latch edit is
still restart-required, since the old path is bound at startup). Only `RUST_LOG`
(`info,tower_http=warn,clip=debug`) is read (a logging default baked into the image, not a
setting).

**clone-daemon:** `RMNG_SOCKET` (media socket; **absent → capture self-test mode**),
`RMNG_CLONE_ID` (id; default hostname), `RMNG_MONITORS` (boot-default layout CSV, below —
corrected to the config's active layout preset by the server's `SetMonitors` on a new clone's
first registration, and thereafter whenever the clone is the selected one),
`RMNG_DAEMON_MCP_PORT` (`9004`), `RMNG_DESKTOP_HEIGHT` (`1080`; height of the MCP's virtual
coordinate/screenshot space — `0` serves everything at native res; see [MCP.md](MCP.md)),
`RMNG_EMBEDDED_CURSOR` (composite cursor into frames
instead of METADATA), `RMNG_DRM_FORMAT` (override DRM fourcc:modifier), `RMNG_NUDGE`
(oscillate cursor to force damage — test only), `RUST_LOG`.

**viewer:** `RMNG_VIDEO` (`host:port` of the control-server video port, default
`127.0.0.1:9001`), `RMNG_DUMP=frame.png` (headless: dump one decoded frame and exit),
`RMNG_CLIP_ECHO=1` (headless: log the first 120 characters of each text clipboard payload,
not just its size), `RMNG_NO_GRAB` (disable pointer grab), `RMNG_NO_POINTER_LOCK` (disable
pointer-lock).

---

## clone-daemon CLI

Source: [clone-daemon/src/main.rs](../crates/clone-daemon/src/main.rs).

**Shipping mode (default, no argument):** if `RMNG_SOCKET` is set, connect to the media
socket, connect to the session holder, capture the monitors it holds, ship dmabuf frames +
cursor, relay input to the holder, and serve the daemon MCP on `:9004`. With no socket it
runs a capture-fps self-test on a session of its own.

**`--session-holder`:** run as the session holder instead. It RecordVirtuals the boot layout,
holds it open across the daemon's restarts, and injects input and clipboard on the daemon's
behalf. See the holder socket protocol above.

**`RMNG_MONITORS` format:** comma-separated `WxH+X+Y[*]` (offset optional; trailing `*` =
primary; first is primary if none marked). E.g. `1920x1080+0+0*,1280x1024+1920+0`. Empty →
one 1920×1080 primary. The unique `WxH` sizes also seed `MUTTER_DEBUG_DUMMY_MODE_SPECS`. This
env var is now only a **boot default** baked into the clone template, read by the session
holder and only when `~/.rmng/monitors` has no remembered layout. The shipped holder unit
carries no layout of its own, one unit going to every clone, so a payload push writes the
active preset into `~/.rmng/monitors` when the file is absent. That is what keeps a clone
upgrading onto the holder from building its first session on the built-in single monitor.

The server corrects a stale layout with `ServerMsg::SetMonitors` carrying
`config.effective_monitors()` (the active layout preset), live and without a restart. That
happens once for a brand-new clone when its daemon first registers, on any `Hello` that
reports a freshly started holder, and after that only while the clone is the one on screen:
its `Hello`, a `POST /api/layout/activate`, and the `POST /api/activate` that selects it.

---

## Per-crate public API

**`wire`** ([lib.rs](../crates/wire/src/lib.rs)) — pure types, no I/O. Modules: `config`
(`AppConfig` & friends, `AppConfigRedacted`), `control` (`ControlState`, `Clone`, `Operation`,
`Chat`/`ChatMessage`, `ClaudeUsage`, `MonitorSpec`, the enums), `socket` (clone-socket
protocol), `viewer` (port-1 logical types), `mcp` (MCP arg DTOs). control + config types
derive `ts_rs::TS` and export to `frontend/app/lib/wire/`.

**`media`** ([lib.rs](../crates/media/src/lib.rs)) — the GPU + socket plane:
- `init() -> Result<()>` — init GStreamer once.
- `Encoder::new(on_au: FnMut(Vec<u8>, bool))` / `.push(fd, fourcc, modifier, w, h)` /
  `.force_idr()` — one VA-API H.264 encoder per monitor (`vapostproc ! vah264enc ! h264parse`,
  Annex-B AUs to the callback).
- `screenshot_jpeg(fd, fourcc, modifier, w, h) -> Vec<u8>` — one-shot dmabuf→JPEG.
- `Listener::bind(path)` / `.accept() -> Conn`; `Conn::recv() -> (DaemonMsg, Vec<OwnedFd>)` /
  `.send(&ServerMsg)` — the clone-socket transport (SCM_RIGHTS).

**`control-client`** ([lib.rs](../crates/control-client/src/lib.rs)) — `Client::new(base)`,
`Client::state_once() -> ControlState`: a thin reqwest+SSE client for integration tests.

**`clone-daemon`, `control-server`, `viewer`** are binaries (no library API); their internal
modules are described in their crate READMEs.
