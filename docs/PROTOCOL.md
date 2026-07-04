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
| web | `9000` | `listen.web` | control-server web | browser / control-client | HTTP + SSE |
| per-clone MCP | `9002` | `listen.clone_mcp` | control-server mcp | in-clone agent-wrapper | HTTP JSON-RPC (header-routed) |
| fleet MCP | `9003` | `listen.global_mcp` | control-server mcp | operator / fleet agents | HTTP JSON-RPC |
| forward | `9005` | `listen.forward` | control-server mediaplane | native viewer | framed TCP over TCP (one conn per forwarded local socket, spliced to the clone) |
| daemon MCP | `9004` | `RMNG_DAEMON_MCP_PORT` | clone-daemon | agent-wrapper + fleet MCP proxy | HTTP JSON-RPC |
| agent-wrapper | `4096` | `agent_port` (config) / `AGENT_PORT` | agent-wrapper (in clone) | control-server chat proxy | HTTP + SSE |
| clone socket | `/srv/rmng-sock/clones.sock` | `cloneSocket` config (server) / `RMNG_SOCKET` (daemon) | control-server mediaplane | clone-daemon | unix `SOCK_SEQPACKET` + `SCM_RIGHTS` |

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
| `pointer_move` | `monitor_id`, `x`, `y` (f64) | absolute pointer in monitor-pixel space |
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

**Handshake:** the daemon's first message is `DaemonMsg::Hello { clone_id }`.

`DaemonMsg` (daemon → server), serde tag `t`:

| Variant | Payload | Meaning |
|---|---|---|
| `hello` | `{clone_id}` | register the clone |
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
then stops the old one — running apps never close). Sent once on the daemon's `Hello` (to
correct a stale baked boot layout) and again on every `POST /api/layout/activate`.

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

---

## Config schema

`AppConfig` loads from `./config.json` in the working directory (no env override — the Docker
image sets `WORKDIR /data`, so `config.json` + `data/` land in the `rmng-data` volume);
written at `0600`. The web API returns `AppConfigRedacted` (the only secret, preset Linear
keys, → `linearKeySet: bool`); `PUT /api/config` returns
`{ config: AppConfigRedacted, restartRequired: bool, networkWarning?: string }`. Source:
[config.rs](../crates/wire/src/config.rs).

| Field | Type | Default | Notes |
|---|---|---|---|
| `listen` | `ListenConfig` | see below | the 6 ports |
| `agent_port` | u16 | `4096` | agent-wrapper port on each clone |
| `data_dir` | string | `"data"` | state/notes/uploads/chats/feedback root; `state.json` and the `claude-accounts.json` secret store live here. **One-time** (set in the setup wizard) |
| `static_dir` | string | `""` (embedded) | empty serves the frontend embedded in the binary; a non-empty disk path serves the bundle from there. Set in Settings → Advanced. **Restart-required** |
| `clone_socket` | string | `/srv/rmng-sock/clones.sock` | media-plane unix socket the clone-daemons connect to; baked into the template at provision. Set in the setup wizard. **One-time** (a pre-latch edit is **restart-required** — the old path is bound at startup) |
| `chroma` | `ChromaMode` | `4:2:0` | viewer video chroma subsampling. Settings → Video. **Restart-required** |
| `setup_complete` | bool | `false` | latched `true` by the first-run setup wizard; gates the frontend to the wizard until then |
| `layout_presets` | `LayoutPreset[]` | `[]` | named monitor-layout presets (`{name, monitors: MonitorSpec[]}`); the operator switches the active one from the sidebar (`POST /api/layout/activate`) |
| `active_layout` | string | `""` | name of the active layout preset; drives `effective_monitors()` (see below) |
| `docker` | `DockerConfig` | see below | daemon socket + `rmng`-network subnet + hostname prefix + per-clone limits |
| `presets` | `Preset[]` | `[]` | clone presets: env vars + Linear key + auto-select labels (**key secret**) |
| `claude` | `ClaudeConfig` | — | usage polling config |
| `clone_groups` | `CloneGroup[]` | `[]` | named account pools for Claude rotation (not secret) |
| `codex` | `CodexConfig` | — | Codex usage polling config |
| `codex_groups` | `CloneGroup[]` | `[]` | named account pools for Codex rotation (not secret) |
| `detector_inference_url` | string | `http://10.0.0.42:8080` | vision-LLM the needs-human detector polls; injected into clones as `RMNG_INFERENCE_URL` |
| `agent_playbook` | string | shipped default | the desktop agent's base playbook (operating notes + ticket procedure), injected into each new clone at creation as its system-prompt append (written to the clone's `~/.config/rmng/agent-instructions.md`, where the agent-wrapper reads it, overriding its baked-in fallback). Seeded from the wrapper's `agent-instructions.md`; editable in Settings; **non-secret** (passes through the redacted view); applies to the next clone (**not restart-required**) |

- **`ListenConfig`**: `web 9000`, `video 9001`, `clone_mcp 9002`, `global_mcp 9003`,
  `daemon_mcp 9004`, `forward 9005`.
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
  `docker.hostname_prefix`; old `state.json` hosts load as plain unmanaged rows
  (`managed: false`; serde drops the stale `ctid`/`container` keys).
- <a id="preset"></a>**`Preset`**: `name`, `labels` (Linear ticket labels that auto-select
  this preset when cloning from a ticket — case-insensitive, first match in config order
  wins), `linear_key` (personal API key, **secret** — fetches/creates tickets server-side
  and is injected into the clone as `LINEAR_API_KEY`, authing its `linear` MCP), `vars`
  (env vars written to the clone's session env), and `agent_playbook` (optional,
  non-secret — text appended after a blank line to the global `agent_playbook` for
  clones of this preset; empty ⇒ global only). `PUT /api/config` merges rows by name
  (blank `linearKey` keeps the stored one; omitted row deletes). One-shot migration at
  load: legacy `envPresets` seed `presets` (no labels/keys); legacy per-workspace `linear`
  keys are dropped (re-enter per preset in Settings).
- **`ClaudeConfig`**: `poll_secs` (`600`, floored 15), `pinned_email?`.
- <a id="claude-accounts"></a>**Claude accounts** live outside config, in the server's 0600
  secret store `claude-accounts.json`: per account an OAuth pair (`access_token` +
  single-use `refresh_token`, both **secret**), harvested from a signed-in clone at import.
  The server owns the whole refresh lifecycle; a clone gets **only the current short-lived
  access token** written into its `~/.claude/.credentials.json` (refresh emptied, far-future
  expiry), re-pushed to every assigned clone whenever a refresh rotates it — so a *running*
  clone hot-swaps without restart (written via `docker exec` into the clone).
- **`CloneGroup`**: `name`, `accounts` (member emails). A clone bound to a group
  (`Host.claude_group`) sticks to its account (preserving its prompt cache) until that
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
- **`Host`** carries `codexAccountEmail` / `codexGroup` / `codexSelection` alongside the
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
corrected to the config's active layout preset by the server's `SetMonitors` on `Hello`),
`RMNG_DAEMON_MCP_PORT` (`9004`), `RMNG_EMBEDDED_CURSOR` (composite cursor into frames
instead of METADATA), `RMNG_DRM_FORMAT` (override DRM fourcc:modifier), `RMNG_NUDGE`
(oscillate cursor to force damage — test only), `RUST_LOG`.

**viewer:** `RMNG_VIDEO` (`host:port` of the control-server video port, default
`127.0.0.1:9001`), `RMNG_DUMP=frame.png` (headless: dump one decoded frame and exit),
`RMNG_NO_GRAB` (disable pointer grab), `RMNG_NO_POINTER_LOCK` (disable pointer-lock).

---

## clone-daemon CLI

Source: [clone-daemon/src/main.rs](../crates/clone-daemon/src/main.rs).

**Shipping mode (default, no subcommand):** if `RMNG_SOCKET` is set, connect to the media
socket, RecordVirtual the `RMNG_MONITORS` boot layout, ship dmabuf frames + cursor, inject
input, and serve the daemon MCP on `:9004`. With no socket it runs a capture-fps self-test.

**`RMNG_MONITORS` format:** comma-separated `WxH+X+Y[*]` (offset optional; trailing `*` =
primary; first is primary if none marked). E.g. `1920x1080+0+0*,1280x1024+1920+0`. Empty →
one 1920×1080 primary. The unique `WxH` sizes also seed `MUTTER_DEBUG_DUMMY_MODE_SPECS`. This
env var is now only a **pre-connect boot default** baked into the clone template — it is not
re-read or hot-reloaded after that. As soon as the daemon's first `Hello` reaches the server,
the server replies with `ServerMsg::SetMonitors` carrying `config.effective_monitors()` (the
active layout preset), live-correcting a stale baked layout without a restart. Every
subsequent `POST /api/layout/activate` pushes a fresh `SetMonitors` the same way.

**`rmng-clone-daemon wait-for-stuck`** — the needs-human detector. Pulls screenshots from the
local MCP, tiles them, asks the inference LLM, exits 0 when stuck. Flags: `--inference-url
<url>` (default the built-in inference CT), `--ignore-reason <str>` (repeatable),
`--interval <secs>` (60), `--timeout <secs>` (1200).

**`clone-daemon report-detection`** — POST a wrong-verdict record to the control-server's
`/api/detector-feedback`. Flags: `--kind false-positive|false-negative` (required), `--note
<str>`, `--control <url>`. (These two subcommands replace the retired `computer-use` binary;
the agent-wrapper spawns `wait-for-stuck` for monitoring.)

---

## Per-crate public API

**`wire`** ([lib.rs](../crates/wire/src/lib.rs)) — pure types, no I/O. Modules: `config`
(`AppConfig` & friends, `AppConfigRedacted`), `control` (`ControlState`, `Host`, `Operation`,
`Chat`/`ChatMessage`, `ClaudeUsage`, `MonitorSpec`, the enums), `socket` (clone-socket
protocol), `viewer` (port-1 logical types), `mcp` (MCP arg DTOs). control + config types
derive `ts_rs::TS` and export to `frontend/app/lib/wire/`.

**`media`** ([lib.rs](../crates/media/src/lib.rs)) — the GPU + socket plane:
- `init() -> Result<()>` — init GStreamer once.
- `Encoder::new(on_au: FnMut(Vec<u8>, bool))` / `.push(fd, fourcc, modifier, w, h)` /
  `.force_idr()` — one VA-API H.264 encoder per monitor (`vapostproc ! vah264enc ! h264parse`,
  Annex-B AUs to the callback).
- `screenshot_png(fd, fourcc, modifier, w, h) -> Vec<u8>` — one-shot dmabuf→PNG.
- `Listener::bind(path)` / `.accept() -> Conn`; `Conn::recv() -> (DaemonMsg, Vec<OwnedFd>)` /
  `.send(&ServerMsg)` — the clone-socket transport (SCM_RIGHTS).

**`control-client`** ([lib.rs](../crates/control-client/src/lib.rs)) — `Client::new(base)`,
`Client::state_once() -> ControlState`: a thin reqwest+SSE client for integration tests.

**`clone-daemon`, `control-server`, `viewer`** are binaries (no library API); their internal
modules are described in their crate READMEs.
