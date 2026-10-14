# Protocol, config & internals reference

Everything below the HTTP/MCP layer: the browser viewer protocol, the clone↔server
unix socket, the config schema, every environment variable, the clone-daemon CLI, and each
crate's public Rust API. Sources: [crates/wire/src/socket.rs](../crates/wire/src/socket.rs),
[browser.rs](../crates/wire/src/browser.rs), [config.rs](../crates/wire/src/config.rs),
[control-server/src/mediaplane.rs](../crates/control-server/src/mediaplane.rs),
[control-server/src/webviewer.rs](../crates/control-server/src/webviewer.rs),
[clone-daemon/src/transport.rs](../crates/clone-daemon/src/transport.rs),
[media/src/sock.rs](../crates/media/src/sock.rs).

## Ports & sockets

| Name | Default | Override | Listener | Connected by | Transport |
| --- | --- | --- | --- | --- | --- |
| web | `9000` | — (`wire::PORT_WEB`) | control-server web | browser (dashboard + viewer) / `rmng` CLI / control-client | HTTP + SSE + WebSocket (`/api/viewer`) |
| bastion | `2222` | — (`wire::PORT_BASTION`) | control-server ssh.rs | operator ssh client (jump) | OpenSSH; pubkey-only; forwards to `clone:22` |
| daemon MCP | `9004` | `RMNG_DAEMON_MCP_PORT` | clone-daemon | agents in the clone + `rmng desktop` proxy (via web API) | HTTP JSON-RPC |
| clone socket | `/srv/rmng-sock/clones.sock` | — (`wire::CLONE_SOCKET`, server) / `RMNG_SOCKET` (daemon) | control-server mediaplane | clone-daemon | unix `SOCK_SEQPACKET` + `SCM_RIGHTS` |
| holder socket | `$XDG_RUNTIME_DIR/rmng-session-holder.sock` | `RMNG_HOLDER_SOCKET` | session holder (in clone) | clone-daemon | unix `SOCK_SEQPACKET` |

---

## Browser viewer protocol (browser ⇄ control-server)

Each viewer window opens one WebSocket on the web port, same origin as the page:
`ws://<host>:9000/api/viewer?monitor=<id>`, or `wss://` when the page is HTTPS. The
socket carries the video of that one monitor and every control message. Source:
[browser.rs](../crates/wire/src/browser.rs) (types),
[webviewer.rs](../crates/control-server/src/webviewer.rs) (socket),
[mediaplane.rs](../crates/control-server/src/mediaplane.rs) (fan-out).

**Binary messages.** Byte 0 is the kind.

Video (kind `0`, server → browser): a 10-byte `VideoHeader`, then one Annex-B access unit.
All numbers are big-endian.

| Bytes | Field |
| --- | --- |
| `0` | kind, always `0` |
| `1` | flags, bit 0 = keyframe |
| `2..6` | monitor id (u32) |
| `6..10` | frame sequence number of the monitor's encoder (u32, wraps) |
| `10..` | the H.264 access unit (Annex-B) |

Clipboard data (kind `1`, both directions): one `ClipboardData` reply, so an image crosses
as raw bytes, not as base64 in JSON (`encode_clipboard_data` / `decode_clipboard_data`).

| Bytes | Field |
| --- | --- |
| `0` | kind, always `1` |
| `1..9` | offer serial (u64) |
| `9..11` | MIME type length `n` (u16) |
| `11..11+n` | MIME type, UTF-8 |
| `11+n..` | the bytes (empty = the owner could not read them) |

**Text messages** are JSON. The `t` field names the message (snake_case).

`ToBrowser` (server → browser):

| `t` | Body | Meaning |
| --- | --- | --- |
| `version` | `{build_id}` | The server's build, first on every connection. A window that reconnects and sees a different one reloads. |
| `view_spec` | `ViewSpec {monitors: ViewMonitor[], content}` | The window set and what it shows. `content.mode` is `desktop`, or `terminal` with `clone` and `sessions` for a clone with no desktop. Sent at connect and on every selection or layout change. Empty `monitors` = nothing is selected. |
| `cursor` | `CursorMeta` | The remote cursor (position, shape on change, `warp`). Each window uses the messages for its own `monitor_id`. |
| `clipboard` | `ClipboardMsg` (`k`: `offer` / `request`) | The clipboard broker. `data` never goes as JSON; it is a binary message. |
| `term_data` | `TermData {session, data /* b64 */}` | Terminal output of one tmux session. |

`FromBrowser` (browser → server):

| `t` | Body | Meaning |
| --- | --- | --- |
| `input` | `InputMsg` (`kind`, below) | An input event for the **selected** clone. |
| `clipboard` | `ClipboardMsg` (`k`: `offer` / `request`) | The window's clipboard offer or request. Data goes as a binary message. |
| `term_input` | `TermInput {session, data /* b64 */}` | Keys or pasted bytes for one tmux session. |
| `term_resize` | `TermResize {cols, rows}` | The terminal size. Every session is resized to it. |
| `term_new_session` | — | Make a new tmux session (the "+" tab). |
| `request_keyframe` | `RequestKeyframe {monitor_id}` | Re-encode the monitor's last frame as a keyframe: at connect and after a decode error. |

**Connect.** The server queues the view spec, the last cursor of each monitor and the
current clipboard offer, then re-encodes the last frame of this monitor as a keyframe. A
selection change sends a new view spec, the cursors and a keyframe to every window.

**Queue and timeouts.** Each socket has a queue of 128 messages. When the queue is full
(the browser does not read fast enough), the server closes the socket. The browser then
reconnects and is primed again. The server sends a ping every 5 s. A send that makes no
progress for 10 s closes the socket. A socket that receives nothing (pongs included) for
10 s is closed. One WebSocket message may be up to 32 MiB (a clipboard image).

**Held input.** When the selection moves to another clone, or the socket closes, the server
releases the keys and buttons this socket still holds down in the old clone.

**File upload.** `POST /api/viewer/upload?name=<file name>` with the raw file as the body
writes the file into the selected clone's home (`~/`). A name that exists gets ` (1)`,
` (2)`… before its extension. The reply is `{name}`, the name used.

`InputMsg` ([socket.rs](../crates/wire/src/socket.rs), serde tag `kind`, snake_case):

| Variant | Fields | Use |
| --- | --- | --- |
| `pointer_move` | `monitor_id`, `x`, `y` (f64) | absolute pointer in monitor-pixel space (**native**, see below) |
| `pointer_relative` | `dx`, `dy` (f64) | unaccelerated delta — pointer-lock / games |
| `button` | `button` (evdev: `0x110`–`0x112` left/right/middle, `0x113`/`0x114` back/forward), `pressed` | mouse button |
| `axis` | `axis` (0=vert,1=horiz), `step` (±1) | discrete scroll (mouse wheel notch) |
| `axis_continuous` | `dx`, `dy`, `flags` | smooth (trackpad) scroll |
| `key` | `keysym` (X11), `pressed` | text/modifier key (MCP `key` path) |
| `key_code` | `keycode` (Linux evdev, from `KeyboardEvent.code`), `pressed` | physical-key identity |

The viewer sends `key_code` for keys, `pointer_move`/`button`/`axis`/`axis_continuous` for
the mouse, and `pointer_relative` while the pointer is locked (Ctrl+Alt+G, or automatically
while the clone hides its cursor). When the server sends a `CursorMeta{warp:true}` (an
MCP-driven move) the viewer snaps the drawn remote cursor and **ignores local motion for
~0.5 s** so the operator's mouse does not fight the agent.

> **This path is native.** Coordinates here are the monitor's real pixels, and the H.264
> stream is encoded at full resolution. The 1080p virtual space is a property of the
> **daemon MCP tool channel only** ([MCP.md](MCP.md)) — the daemon converts virtual → native
> before it ever reaches this socket. Do not change the viewer or the encoder to 1080p.

**Frame-rate test.** `rmng-fps` ([rmng-fps.rs](../crates/control-client/src/bin/rmng-fps.rs))
connects to `/api/viewer` like a viewer window and prints once a second how many frames,
keyframes and megabits arrived. It does not decode.

---

## Clone socket protocol (clone-daemon ⇄ control-server)

A unix `SOCK_SEQPACKET` socket (one JSON message per datagram). dmabuf file descriptors ride
out-of-band via `SCM_RIGHTS` in the same datagram, in plane order — never in the JSON. The
daemon connects to `RMNG_SOCKET`; the server listens on `wire::CLONE_SOCKET`
(`/srv/rmng-sock/clones.sock`, a constant, written into every clone's `RMNG_SOCKET` at
bootstrap). The socket lives in
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
| --- | --- | --- |
| `hello` | `{clone_id, fresh_session}` | register the clone; `fresh_session` asks for the active layout |
| `frame` | `FrameMsg` | one captured monitor frame; dmabuf fds attached via SCM_RIGHTS |
| `cursor` | `CursorMeta` | cursor position (+shape on change, +`warp` if MCP-driven) |
| `layout` | `{monitors: MonitorPlacement[]}` | the monitors the clone has after it applies a layout (`{id}` each); the server drops the encoders of monitors that are gone |
| `clipboard_offer` / `clipboard_request` / `clipboard_data` | resp. types | clipboard bridge |

`ServerMsg` (server → daemon), serde tag `t`: `ack
{monitor_id, seq}` (flow control — the daemon waits for the ack before the next frame),
`input(InputMsg)`, the three `clipboard_*` messages, `capture {active}` — start or stop
the clone's capture (the server keeps it on only for the selected clone while a viewer is
connected), and `set_monitors {monitors:
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
transfers. `ClipboardMsg` (serde tag `k`: `offer`/`request`/`data`) is the viewer-side
envelope. The control-server is the **broker**: it tracks the owner, fans each offer to
every viewer window + every other clone, routes a paste's request to the owner, and routes
bytes back to the requester. The clone-daemon bridges via Mutter `RemoteDesktop` selection
(`SelectionRead`/`SelectionWrite`); the browser viewer via the async clipboard API, which
Chrome allows only while the window has focus.

**An image does not fit in one message, on either hop.** Both had a ceiling sized for text,
and a pasted screenshot crossed neither.

The clone socket is `SOCK_SEQPACKET`, and the kernel refuses a datagram over `SO_SNDBUF - 32`,
about 208 KB at the default `net.core.wmem_default`. A 256 KB payload is 349,596 bytes of JSON,
so `sendmsg` returned `EMSGSIZE` and the broker discarded the error. A message past the ceiling
is now split into 64 KiB chunks framed `\0RMC | id | index | count | slice` and rejoined by the
receiver. The magic starts with a NUL, which no JSON document can, so a whole message is still
sent exactly as it was and needs no envelope. The receive queue holds about three chunks, so a
chunked send waits for the peer to drain between datagrams, bounded at 10 s.

On the viewer socket, clipboard bytes go as one binary WebSocket message of up to 32 MiB.

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
injection and the clipboard bridge. `rmng-clone-daemon.service` owns capture, shipping,
screenshots, and the MCP on `:9004`. Video encode runs on the control-server. Both are the same binary, the holder started with
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
| --- | --- | --- |
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
`{ config: AppConfigRedacted, networkWarning?: string }`. Source:
[config.rs](../crates/wire/src/config.rs).

Ports, the data directory, the clone socket and the Docker socket are constants in
`wire::config` (`PORT_WEB`, `PORT_BASTION`, `PORT_DAEMON_MCP`, `DATA_DIR`, `CLONE_SOCKET`,
`DOCKER_SOCKET`), not config fields. Keys from older files (`listen`, `dataDir`,
`staticDir`, `cloneSocket`, `chroma`) are ignored when `config.json` loads.

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `setup_complete` | bool | `false` | latched `true` by the first-run setup wizard; gates the frontend to the wizard until then |
| `layout_presets` | `LayoutPreset[]` | `[]` | named monitor-layout presets (`{name, monitors: MonitorSpec[]}`); the operator switches the active one from the sidebar (`POST /api/layout/activate`) |
| `active_layout` | string | `""` | name of the active layout preset; drives `effective_monitors()` (see below) |
| `docker` | `DockerConfig` | see below | hostname prefix + per-clone limits + ZFS parent dataset for clone homes |
| `presets` | `Preset[]` | `[]` | clone presets: env vars + Linear key + auto-select ticket-id prefixes (the key is a credential, and `GET /api/config` vends it to the clients) |
| `claude` | `ClaudeConfig` | — | usage polling config |
| `groups` | `CloneGroup[]` | `[]` | the single named account-pool list (Claude + Codex members mixed); a clone binds at most one pool, each rotator side only sees its own provider members (not secret) |
| `agent_playbook` | string | shipped default | the person's ticket procedure and app quirks for the assistant (how to drive a desktop is the assistant's own knowledge), sent with the preset's append in the `[From RMNG]` header of each new assistant chat's first message (see [API.md](API.md#per-clone-assistant-chat)); nothing is written into the clone. Seeded from [agent-playbook.md](../crates/wire/src/agent-playbook.md); editable in Settings; **non-secret** (passes through the redacted view); applies to the next new chat |
| `judge` | `JudgeConfig` | `gpt-5.6-luna` | working-vs-stuck detection: `codexModel` (default `gpt-5.6-luna`) and `codexEmail` (`null` = the first imported Codex account). No credential of its own: the calls run on that account's ChatGPT plan, using the token the server already holds to run its clones. Nothing secret, so the whole struct passes through the redacted view. No Codex account imported ⇒ no clone is ever reported `working` (see [monitorState](API.md#monitorstate)). Settings → Agents |
| `assistant` | `AssistantConfig` | `{url: "", serverUrl: ""}` | the chat panel's assistant: `url` is the pi-web origin, `serverUrl` this server's web API origin as the assistant reaches it (written into each new chat's first message for `rmng --server`). Empty `url` ⇒ the chat panel refuses to send; empty `serverUrl` ⇒ it refuses to start a new chat. Plain addresses, so the struct passes through the redacted view. Settings → Presets → Assistant |

- **`DockerConfig`** (no secret, so the whole struct passes through the redacted view):
  `hostname_prefix` (`"pega-"`, editable in Settings → prepended to derived clone hostnames;
  carried from the retired `proxmox.hostname_prefix` on migration), `clone_cpus` (`16` —
  whole cores → `nano_cpus`) and `clone_memory_mb` (`32768` — MiB, +8 GiB swap), both
  editable per-clone limits, and `homes_parent` (the parent ZFS dataset of the clone homes).
  The Docker socket is the constant `wire::DOCKER_SOCKET` (`/var/run/docker.sock`) and the
  `rmng` bridge subnet is the constant `wire::DOCKER_SUBNET` (`10.99.0.0/24`; addressing is
  Docker DNS — clones resolve by container name, the control-server by its `rmng-control`
  alias). Retired keys in an old file (`socket`, `subnet`, `templateReference`, …) are
  ignored on load.
- **First-run setup wizard**: a fresh deploy ships `config.json` with `"setupComplete":
  false`, so the web UI shows the wizard (environment checklist → server settings + monitors
  → finish) instead of the dashboard; finishing latches
  `setupComplete: true` (a one-way latch) and materializes the lazy `rmng` network. There is
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
  windows map to 5h/weekly by `limit_window_seconds`.
- **`CodexConfig`**: `auto_reset` (bool, default `false`).
- **`groups`** (`CloneGroup[]`): the single pool list above. A clone binds one pool via its
  clone-level `group` (each side resolving to its own provider's least-used member); a legacy
  per-side `group:<name>` selection still binds it. Selected at clone/swap time as the shared
  `group` field.
- **`Clone`** carries `codexAccountEmail` / `codexGroup` / `codexSelection` alongside the
  Claude equivalents. One clone can hold both a Claude and a Codex account simultaneously.

- **`MonitorSpec`**: `width`, `height`, `x`, `y`, `primary`.
- **`LayoutPreset`**: `name`, `monitors: MonitorSpec[]` — a full named arrangement the
  operator can switch to live. `AppConfig::effective_monitors()` returns the active preset's
  (`active_layout`) monitors, falling back to the first preset, then a built-in dual
  2560×1440 default when `layout_presets` is empty.

Template params are mostly not config: the base OS is fixed in the template build
(`ubuntu:26.04` in `template/Dockerfile` — the patched gnome-shell is compiled against 26.04's
GNOME only). Clone images build on demand from each preset's Dockerfile into a hash tag
(`POST /api/images/prebuild` warms one without creating). Per-clone CPU / memory limits come
from `docker.clone_cpus` / `docker.clone_memory_mb`, applied at clone create — not per image.

---

## Environment variables

**control-server:** all config is `./config.json` in the working directory (the Docker
image sets `WORKDIR /data`, the `rmng-data` volume). Ports, the clone socket and the Docker
daemon socket are constants in `wire::config`. It reads these env vars, none of which is a
setting:

- `RUST_LOG` — log filter; default `info,tower_http=warn,clip=debug`, also set in the image.
- `RMNG_ENC_LATENCY` — when set, the encoder logs the time from frame push to encoded frame.
  For measurement only.
- `RMNG_CODEX_ACCOUNTS_FILE` — the path of the Codex account store, in place of
  `codex-accounts.json` in the data directory. A test uses it.

The ignored test `gpt_answers_the_real_question` also reads `RMNG_CODEX_AUTH` (the path of a
Codex `auth.json`); the server does not.

**clone-daemon:** `RMNG_SOCKET` (media socket; required unless `--session-holder`),
`RMNG_CLONE_ID` (id; default hostname), `RMNG_MONITORS` (boot-default layout CSV, below —
corrected to the config's active layout preset by the server's `SetMonitors` on a new clone's
first registration, and thereafter whenever the clone is the selected one),
`RMNG_DAEMON_MCP_PORT` (`9004`), `RMNG_DESKTOP_HEIGHT` (`1080`; height of the MCP's virtual
coordinate/screenshot space — `0` serves everything at native res; see [MCP.md](MCP.md)),
`RMNG_HOLDER_SOCKET` (holder socket path; default
`$XDG_RUNTIME_DIR/rmng-session-holder.sock`), `RUST_LOG`.

---

## clone-daemon CLI

Source: [clone-daemon/src/main.rs](../crates/clone-daemon/src/main.rs).

**Shipping mode (default, no argument):** connect to the media socket named by
`RMNG_SOCKET`, connect to the session holder, capture the monitors it holds, ship dmabuf
frames + cursor, relay input to the holder, and serve the daemon MCP on `:9004`. Without
`RMNG_SOCKET` it exits with an error.

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
protocol), `browser` (browser viewer protocol), `mcp` (MCP arg DTOs). control, config and
browser types derive `ts_rs::TS` and export to `frontend/app/lib/wire/`.

**`media`** ([lib.rs](../crates/media/src/lib.rs)) — the GPU + socket plane:

- `Encoder::new(on_au: FnMut(Vec<u8>, bool))` / `.push(fd, fourcc, modifier, w, h)` /
  `.force_idr()` — one VA-API H.264 encoder per monitor (`vapostproc ! vah264enc ! h264parse`,
  Annex-B AUs to the callback).
- `screenshot_jpeg(fd, fourcc, modifier, w, h) -> Vec<u8>` — one-shot dmabuf→JPEG.
- `Listener::bind(path)` / `.accept() -> Conn`; `Conn::recv() -> (DaemonMsg, Vec<OwnedFd>)` /
  `.send(&ServerMsg)` — the clone-socket transport (SCM_RIGHTS).

**`control-client`** ([lib.rs](../crates/control-client/src/lib.rs)) — `Client::new(base)`,
`Client::state_once() -> ControlState`: a thin reqwest+SSE client for integration tests.
Its `rmng-fps` binary is the viewer frame-rate test (see the browser viewer protocol above).

**`clone-daemon`, `control-server`** are binaries (no library API); their internal
modules are described in their crate READMEs.
