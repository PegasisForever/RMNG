# wire

The shared type crate — the single source of truth for everything that crosses a
process boundary in `RMNG`:

1. **Control state** broadcast over `/events` (port 2) and persisted to `state.json`.
2. The **clone-daemon ⇄ control-server unix-socket protocol** (dmabuf frame
   descriptors, input events, acks).
3. The **viewer protocol** (port 1): framed H.264 + cursor/clipboard out, input in.
4. **MCP DTOs** for the per-clone (port 3) and global (port 4) MCP tool schemas.

Control-plane types derive `serde::{Serialize, Deserialize}` and `ts-rs::TS` so the
**frontend's TypeScript types are generated** from this crate (eliminating the
hand-synced `control-server/app/lib/types.ts` drift that exists today). Internal
transport types (socket, viewer) are serde-only.

## Responsibilities

- Define `ControlState` and its members as the full superset of the current Rust
  `../../shared/src/lib.rs` (`ControlState`, `HostEntry`, `MonitorSpec`) and the current
  TS `../../control-server/app/lib/types.ts` extras (`Host` server-only fields,
  `Operation`, `ClaudeUsage`, `Chat`/`ChatMessage`, `templates`).
- Define the **socket** protocol (`FrameMsg`, `InputMsg`, `Ack`, monitor descriptors).
- Define the **viewer** protocol (server→viewer video/cursor/clipboard/monitor-list;
  viewer→server input/clipboard/keyframe-request/hello).
- Define **MCP DTOs** (tool inputs/outputs shared by ports 3 and 4; port-4 variants add a
  `clone` selector).
- Emit `.ts` bindings consumed by `frontend`.

## Key types (to define)

```text
ControlState { selected: Option<String>, monitors: Vec<MonitorSpec>,
               hosts: Vec<Host>, operations: Vec<Operation>,
               templates: Vec<String>, claude_accounts: Vec<ClaudeUsage>,
               codex_accounts: Vec<CodexUsage> }
Host { id, host, port, username, password, domain?, gdm_username?, gdm_password?,
       container?, source?, claude_account_email?, linear_*?, display_name?,
       agent_report?, state_note?, monitor_state?,
       codex_account_email?, codex_group?, codex_selection? }
Operation { id, kind: Clone|Delete|Bootstrap|Commit, target, source?, status, step, pct,
            message, log: Vec<String>, container?, started_at, finished_at? }
ClaudeUsage { id, email, provider, active, assignable?, error?, stale?,
              last_updated, five_hour?, seven_day?, spend? }
MonitorSpec { width, height }

# configuration (edited via the Settings UI, not hand-edited files)
AppConfig { docker{socket, subnet, hostname_prefix, clone_cpus, clone_memory_mb, template_reference},
            presets: [{name, labels: [label], linear_key, vars: [{key, value}]}],
            claude{poll, pinnedEmail, swap...},
            clone_groups: [{name, accounts: [email]}],
            codex{pollSecs, pinnedEmail, usagePolling},
            codex_groups: [{name, accounts: [email]}],
            clone_socket, data_dir, static_dir, chroma, setup_complete, detector_inference_url,
            monitors: [MonitorSpec], listen{video, web, clone_mcp, daemon_mcp}, agent{port} }
# Clone sources are images (identified by their own repo:tag, e.g. pegasis0/rmng-template:latest),
# not a config template block — pulled from a registry (docker.template_reference, default
# pegasis0/rmng-template:latest) via POST /api/images/pull; see ImageInfo below + docs/API.md.
# A preset's `labels` auto-select it when cloning from a Linear ticket; `linear_key`
# fetches/creates tickets server-side and is injected into the clone as LINEAR_API_KEY.
# Claude account tokens are NOT config: each account's OAuth pair lives in the server's
# 0600 `claude-accounts.json`; the server refreshes it and pushes the current short-lived
# access token into assigned clones' ~/.claude/.credentials.json (see control-server).
# Codex account tokens are NOT config: each account's OAuth triple lives in the server's
# 0600 `codex-accounts.json`; the server refreshes it and pushes a short-lived
# auth.json into assigned clones' ~/.codex/auth.json (refresh_token emptied; see control-server).
CodexConfig { pollSecs, pinnedEmail?, usagePolling: bool }
             # usagePolling=false suppresses GET /wham/usage; refresh + push still run
AppConfigRedacted   # GET /api/config shape: the one secret → set/unset, never plaintext
ImageInfo   # GET /api/images row: {id, reference, size_bytes, created_at, base, created_from?, in_use_by}
SetupEnv / EnvCheckRow   # GET /api/setup/env: the wizard's environment preflight rows
# The only secret is the preset linear key (the Docker backend has none — local unix socket):
# write-only, redacted on read, omitted-keeps-stored on write, NEVER placed in ControlState/SSE.

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

# viewer protocol (port 1: native GTK viewer ⇄ control-server)  [length-prefixed frames]
ViewerHello { token?, capabilities }                  # viewer → server
MonitorList { monitors: [{id, x, y, width, height}] }  # server → viewer
VideoAu { monitor_id, idr: bool, pts, annexb }         # server → viewer (H.264 access unit)
CursorMeta { monitor_id, x, y, hotspot, shape? }       # server → viewer (drawn client-side)
ViewerInput { monitor_id, InputMsg-like }              # viewer → server
RequestKeyframe { monitor_id }                         # viewer → server (reconnect/seek)
# clipboard: same Offer/Request/Data triple as the socket, both directions through the broker
ClipboardOffer/ClipboardRequest/ClipboardData { … }   # viewer ⇄ server

# MCP DTOs (ports 3 + 4) — JSON-RPC tool inputs/outputs
Screenshot{} Click{x,y,button} Type{text} Key{keysym} Move{x,y} Scroll{dx,dy} ...
# Port-4 tool variants embed `clone: String` (which clone to target); port-3 derives
# the clone from the caller's source IP.
```

Field names/casing must match the wire format the existing `/events` consumers expect
(camelCase JSON for the control-plane types — `#[serde(rename_all = "camelCase")]`).
Internal socket/viewer types may use snake_case.

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
- Round-trip the binary socket/viewer headers.
- `ts-rs` export produces TS that type-checks in `frontend`.

## Open questions

- Whether to split into `wire-control` (ts-rs) / `wire-media` (serde-only) so the ts-rs
  export covers only control-plane types.
- Binary framing detail for port 1 (fixed header + length prefix vs a small varint codec).
