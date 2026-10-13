# wire

The shared type crate — the single source of truth for everything that crosses a
process boundary in `RMNG`:

1. **Control state** broadcast over `/events` (port 9000) and persisted to `state.json`.
2. The **clone-daemon ⇄ control-server unix-socket protocol** (dmabuf frame
   descriptors, input events, acks).
3. The **browser viewer protocol** (`browser`, a WebSocket on port 9000): binary H.264 +
   JSON view spec/cursor/clipboard/terminal out, input/clipboard/terminal in.
4. **MCP DTOs** for the clone-local desktop-automation tool schemas.

Control-plane types derive `serde::{Serialize, Deserialize}` and `ts-rs::TS` so the
**frontend's TypeScript types are generated** from this crate (eliminating the
hand-synced `control-server/app/lib/types.ts` drift that exists today). The browser
viewer's messages (`ToBrowser`, `FromBrowser` and the types they carry) are exported too.
The other transport types (socket, holder) are serde-only.

## Responsibilities

- Define `ControlState` and its members as the full superset of the current Rust
  `../../shared/src/lib.rs` (`ControlState`, `HostEntry`, `MonitorSpec`) and the current
  TS `../../control-server/app/lib/types.ts` extras (`Clone` server-only fields,
  `Operation`, `ClaudeUsage`, `Chat`/`ChatMessage`, `templates`).
- Define the **socket** protocol (`FrameMsg`, `InputMsg`, `Ack`, monitor descriptors).
- Define the **browser viewer** protocol (`VideoHeader`, the clipboard binary layout,
  `ToBrowser`: view spec/cursor/clipboard/terminal data; `FromBrowser`:
  input/clipboard/terminal input/keyframe request).
- Define **MCP DTOs** for clone-local desktop automation; the control-server's web API proxies
  operator requests to that clone-local surface.
- Emit `.ts` bindings consumed by `frontend`.

## Key types (to define)

```text
ControlState { selected: Option<String>, monitors: Vec<MonitorSpec>,
               hosts: Vec<RmngClone>, operations: Vec<Operation>,
               templates: Vec<String>, claude_accounts: Vec<ClaudeUsage>,
               codex_accounts: Vec<CodexUsage> }
RmngClone { id, host, port, username, password, domain?, gdm_username?, gdm_password?,
       container?, source?, claude_account_email?, linear_*?, display_name?,
       monitor_state?, unread?,
       codex_account_email?, codex_group?, codex_selection? }
Operation { id, kind: Clone|Delete|Archive|Unarchive|Pull|Prebuild|Update,
            target, source?, status, step, pct,
            message, log: Vec<String>, container?, started_at, finished_at? }
ClaudeUsage { id, email, provider, active, assignable?, error?, stale?,
              last_updated, five_hour?, seven_day?, spend? }
MonitorSpec { width, height }

# configuration (edited via the Settings UI, not hand-edited files)
AppConfig { docker{hostname_prefix, clone_cpus, clone_memory_mb, homes_parent},
            presets: [{name, labels: [label], linear_key, vars: [{key, value}]}],
            claude{poll, pinnedEmail, swap...},
            clone_groups: [{name, accounts: [email]}],
            codex{autoReset},
            codex_groups: [{name, accounts: [email]}],
            setup_complete, layout_presets, active_layout, ssh, ... }
# Ports, data dir and sockets are constants (PORT_WEB, PORT_BASTION, PORT_DAEMON_MCP,
# DATA_DIR, CLONE_SOCKET, DOCKER_SOCKET), not config. Old keys in config.json are ignored.
# Clone images are gen-2 preset builds (each preset's Dockerfile into a hash tag, built on
# demand); the stock base is pegasis0/rmng-template:latest. See docs/API.md + docs/DEPLOY.md.
# A preset's `labels` auto-select it when cloning from a Linear ticket; `linear_key` is what
# the clients fetch and create tickets with, and is injected into the clone as LINEAR_API_KEY.
# Claude account tokens are NOT config: each account's OAuth pair lives in the server's
# 0600 `claude-accounts.json`; the server refreshes it and pushes the current short-lived
# access token into assigned clones' ~/.claude/.credentials.json (see control-server).
# Codex account tokens are NOT config: each account's OAuth triple lives in the server's
# 0600 `codex-accounts.json`; the server refreshes it and pushes a short-lived
# auth.json into assigned clones' ~/.codex/auth.json (refresh_token emptied; see control-server).
CodexConfig { autoReset: bool }
AppConfigRedacted   # GET /api/config shape: same fields, preset linear keys included
SetupEnv / EnvCheckRow   # GET /api/setup/env: the wizard's environment preflight rows
# The only credential is the preset linear key (the Docker backend has none — local unix
# socket). GET /api/config returns it verbatim: the browser lists Linear issues itself,
# and this server is Tailscale-only. Write-only on the way back (blank keeps stored), and it
# stays out of ControlState/SSE, which is a different document with a different lifetime.

# socket protocol (clone-daemon ⇄ control-server, SOCK_SEQPACKET + SCM_RIGHTS)
FrameMsg { monitor_id, fourcc, modifier, width, height, planes: [{stride, offset}], seq }
           # + dmabuf fds via SCM_RIGHTS.  Cursor is NOT composited — see CursorMeta.
CursorMeta { monitor_id, x, y, hotspot, shape?: {w, h, rgba} }  # cursor-mode METADATA;
           # shape sent only when it changes (daemon captures shape updates separately).
InputMsg { monitor_id, kind: PointerMove|Button|Axis|Key, ... }
# Key = X11 keysym + pressed; Button = evdev index; PointerMove = absolute per-monitor.
Ack { monitor_id, seq }
Subscribe { stream: bool }   # server tells the daemon to start/stop the continuous feed
FrameRequest { }             # server asks for one frame on demand (screenshot path)
# clipboard (rich + lazy): daemon ⇄ server, broker = control-server
ClipboardOffer  { serial, mime_types: [String] }      # "selection changed; these types"
ClipboardRequest{ serial, mime_type }                  # "give me bytes for this type"
ClipboardData   { serial, mime_type, bytes }           # the bytes (lazy fetch)

# browser viewer protocol (WebSocket /api/viewer?monitor=<id> on port 9000)
VideoHeader { monitor_id, seq, key, server_us }        # binary, 18 bytes + Annex-B AU
encode_clipboard_data / decode_clipboard_data          # binary ClipboardData, both ways
ToBrowser   (JSON, tag "t"): view_spec(ViewSpec) | cursor(CursorMeta)
            | clipboard(ClipboardMsg) | term_data(TermData)
FromBrowser (JSON, tag "t"): input(InputMsg) | clipboard(ClipboardMsg) | term_input
            | term_resize | term_new_session | request_keyframe{monitor_id}
ViewSpec { monitors: [ViewMonitor{id, x, y, width, height, primary}],
           content: desktop | terminal{clone, sessions} }

# Clone-local daemon MCP DTOs — JSON-RPC tool inputs/outputs
Screenshot{} Click{x,y,button} Type{text} Key{keysym} Move{x,y} Scroll{dx,dy} ...
# The daemon serves this surface on localhost; the control-server's desktop API proxies to it.
```

Field names/casing must match the wire format the existing `/events` consumers expect
(camelCase JSON for the control-plane types — `#[serde(rename_all = "camelCase")]`).
Internal socket and browser viewer types may use snake_case.

## Contract preservation

- Serialize `ControlState` so the frontend keeps parsing it unchanged
  (unknown-field-tolerant; only the **server** publisher changes). This is the one wire
  contract we deliberately keep across the cutover; the Rust types themselves are written
  fresh (see [Clean-room](../../docs/DEVELOPMENT.md#clean-room)).
- During cutover the old `../../shared` crate still backs the legacy client until it is
  retired; `wire` does not depend on it.

## Dependencies

`serde`, `serde_json`, `ts-rs`. No async, no I/O — pure data + (de)serialization. (Frame
payloads are carried as byte slices; `wire` defines headers/descriptors, not codecs.)

## Tests

- Round-trip JSON for every control-plane type; golden-file the `ControlState` JSON shape
  against a capture from the current Bun server.
- Round-trip the binary socket and browser viewer headers (`VideoHeader`, clipboard data).
- `ts-rs` export produces TS that type-checks in `frontend`.

## Open questions

- Whether to split into `wire-control` (ts-rs) / `wire-media` (serde-only) so the ts-rs
  export covers only control-plane types.
