# MCP reference — the three desktop/fleet automation surfaces

RMNG exposes **three** MCP servers, all speaking **JSON-RPC 2.0 over HTTP POST** at
path `/`. Two are on the control-server; one is in each clone-daemon.

| Server | Where | Default port | Scope | Source |
|---|---|---|---|---|
| **per-clone MCP** | control-server | `9002` (`listen.clone_mcp`) | `set_state` only; clone resolved from the **caller's IP** | [control-server/src/mcp.rs](../crates/control-server/src/mcp.rs) |
| **fleet MCP** | control-server | `9003` (`listen.global_mcp`) | every web action + every desktop/window tool (with a `clone` arg); desktop tools **proxied** to the clone's daemon MCP | [control-server/src/mcp.rs](../crates/control-server/src/mcp.rs) |
| **daemon MCP** | each clone-daemon | `9004` (`RMNG_DAEMON_MCP_PORT`) | the **full desktop-automation surface** (input/screenshot/window-mgmt) | [clone-daemon/src/mcp.rs](../crates/clone-daemon/src/mcp.rs), [windows.rs](../crates/clone-daemon/src/windows.rs) |

**Who calls what:**
- The **in-clone agent-wrapper** calls its clone's **daemon MCP** directly on
  `http://127.0.0.1:9004` for desktop actions, and the control-server's **per-clone MCP**
  (`:9002`) for `set_state`.
- **Operators / fleet agents** call the **fleet MCP** (`:9003`) with a `clone` selector; it
  runs web actions locally and proxies desktop/window tools to `http://{clone-ip}:9004`.

## JSON-RPC envelope (all servers)

Request:
```json
{ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
  "params": { "name": "<tool>", "arguments": { } } }
```
Success: `{ "jsonrpc":"2.0", "id":1, "result": { "content": [ … ] } }`.
Error: `{ "jsonrpc":"2.0", "id":1, "error": { "code": -32000, "message": "…" } }`.

Core methods on every server: `initialize` → `{protocolVersion, capabilities:{tools:{}},
serverInfo}`, `ping` → `{}`, `tools/list` → `{tools:[…]}`, `tools/call` → `{content:[…]}`.

Tool result `content` items are either `{ "type":"text", "text":"…" }` or
`{ "type":"image", "mimeType":"image/png", "data":"<base64>" }`. Most desktop tools return a
**post-action screenshot** (image) after a ~350 ms settle.

---

## Per-clone MCP (control-server `:9002`)

Header-routed: the caller self-identifies with its clone id (== hostname) in the
`x-rmng-clone` header — the agent-wrapper sets it on its MCP server config; no `clone`
argument. Exposes exactly one tool.

### `set_state` — `{ report?: "working"|"idle", note?: string }`
Record the agent's desktop verdict + note for the calling clone (sets `agentReport` /
`stateNote`, visible on the dashboard). Returns `"state updated for {clone}"`. Errors if the
caller IP matches no host.

---

## Fleet MCP (control-server `:9003`)

Every tool takes an explicit `clone` (the host id), except the parameterless ones.
**Local** tools run in the control-server; **proxied** tools are forwarded verbatim to the
clone's daemon MCP (`is_daemon_tool` → POST to `http://{host}:{daemon_mcp}/`). A missing
clone → `"unknown clone"`; an unreachable daemon → `"clone-daemon MCP unreachable at …"`.

### Local (web/fleet) tools

| Tool | Args | Does |
|---|---|---|
| `list_hosts` | — | JSON of `hosts[]` + state |
| `select` | `clone` | set the viewer's selected host |
| `clone` | `image`, `hostname` | clone from a source image (`rmng/template:<name>`) → `"clone started: op …"` |
| `delete` | `clone` | delete a host → `"delete started: op …"` |
| `claude_recommended` | — | `{email}` recommended account |
| `claude_swap` | `clone`, `account?` (email/`auto`) | hot-swap the clone's Claude account |
| `set_state` | `clone`, `report?`, `note?` | as per-clone, but clone from the arg |
| `send_message` | `clone`, `text` | send a chat message to the clone's host agent (async — the turn runs detached; `409`-style error if one is already running) → `"message sent to …"` |
| `read_chat` | `clone` | the host-agent chat history + live working state: `{ busy, activity?, messages[] }` |

> There is no `redeploy` tool (and no `/api/clone/redeploy` endpoint) any more — clone
> binaries hot-swap themselves automatically. See
> [DEPLOY.md#upgrades](DEPLOY.md#upgrades).

### Proxied desktop/window tools

`screenshot`, `list_monitors`, `mouse_move`, `left_click`, `right_click`, `middle_click`,
`left_double_click`, `scroll`, `key`, `type`, `list_windows`, `list_apps`, `launch_app`,
`move_window` — same arguments as the daemon MCP below, **plus a required `clone`**. Each is
forwarded to that clone's daemon MCP and the result is returned unchanged.

---

## Daemon MCP (clone-daemon `:9004`)

Runs inside each clone, sharing the daemon's live Mutter `RemoteDesktop` session (input),
its per-monitor latest-dmabuf (screenshots via `media::screenshot_png`), and gnome-shell
`org.gnome.Shell.Eval` (window management). No `clone` argument — it *is* one clone.

### Input & capture tools

| Tool | Args | Behaviour |
|---|---|---|
| `list_monitors` | — | `[{id,width,height}]` |
| `screenshot` | `monitor?`=0 | PNG of the monitor's latest captured frame (image). Errors if no frame buffered yet |
| `mouse_move` | `x`, `y`, `monitor?`=0 | clamp to monitor bounds, eased glide (10 steps ≈100 ms); **emits a cursor-warp** to the viewer each step; settle + screenshot |
| `left_click` / `right_click` / `middle_click` | `x?`, `y?`, `monitor?`=0 | optional eased glide to x,y, then press (`0x110`/`0x111`/`0x112`) → 50 ms → release; settle + screenshot |
| `left_double_click` | `x?`, `y?`, `monitor?`=0 | optional eased glide to x,y, then two left presses 80 ms apart; settle + screenshot |
| `scroll` | `amount` (clamped ±15), `x?`, `y?`, `monitor?`=0 | optional eased glide to x,y, then `amount` discrete vertical notches 25 ms apart; settle + screenshot |
| `key` | `keys` (e.g. `"ctrl+c"`, `"alt+Tab"`, `"Return"`) | parse combo → press in order, release in reverse (X11 keysyms); settle + screenshot |
| `type` | `text` | per-char keysym press/release, 12 ms apart (full Unicode); returns `"typed N chars"` |

**Key combos** ([keysym.rs](../crates/clone-daemon/src/keysym.rs)): `+`-separated; modifiers
`ctrl|control|shift|alt|super|meta|win|cmd`; named keys `Return|Enter|Tab|space|BackSpace|
Delete|Escape|Home|End|Left|Up|Right|Down|Page_Up|Page_Down|F1..F12|XF86Audio*` (+ more);
single chars map Latin-1 directly, others as `0x01000000 | codepoint`.

**Cursor warp:** after an MCP-driven absolute move the daemon emits
`CursorMeta { warp: true }` so the viewer snaps the drawn remote cursor to the agent's
target and suppresses the user's local motion for ~0.5 s (no fighting). See
[PROTOCOL.md](PROTOCOL.md#cursormeta).

### Window-management tools (need the shell-03 `Eval` patch)

Dispatched to gnome-shell via `org.gnome.Shell.Eval` ([windows.rs](../crates/clone-daemon/src/windows.rs)).
If the patch isn't applied, these return a clear "unsafe_mode off — needs shell-03-enable-eval"
error (see [gnome-patch](../gnome-patch/README.md)).

| Tool | Args | Returns |
|---|---|---|
| `list_windows` | — | `[{id,title,wm_class,monitor,on_primary,workspace,maximized,minimized,fullscreen,focus,frame{x,y,width,height}}]` |
| `list_apps` | — | `[{id,name,description}]` (installed launcher apps, sorted) |
| `launch_app` | `id` (e.g. `"firefox.desktop"`) | `{id,name}` |
| `move_window` | `id`, `monitor?`, `mode?`=`maximize`\|`center-half` | unminimize/unfullscreen, move to monitor, then maximize or center-half (1080p/720p); `{id,monitor}` |

### Timing constants
`MOVE_STEPS=10`, `MOVE_STEP_MS=10`, `CLICK_PRESS_MS=50`, `DOUBLE_GAP_MS=80`, `TYPE_KEY_MS=12`,
`SCROLL_STEP_MS=25`, `SETTLE_MS=350` ([mcp.rs](../crates/clone-daemon/src/mcp.rs)).

---

## curl examples

```sh
# Fleet MCP: list every tool
curl -s localhost:9003/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq

# Fleet MCP: screenshot clone "rmng-e2e" monitor 0 (proxied to its daemon)
curl -s localhost:9003/ -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,
  "method":"tools/call","params":{"name":"screenshot","arguments":{"clone":"rmng-e2e","monitor":0}}}'

# Daemon MCP (on the clone): left-click at 640,480
curl -s localhost:9004/ -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,
  "method":"tools/call","params":{"name":"left_click","arguments":{"x":640,"y":480}}}'

# Daemon MCP: type text
curl -s localhost:9004/ -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,
  "method":"tools/call","params":{"name":"type","arguments":{"text":"hello"}}}'

# Per-clone MCP (called from inside the clone): report state
curl -s localhost:9002/ -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,
  "method":"tools/call","params":{"name":"set_state","arguments":{"report":"working"}}}'
```

> The control-server's readiness ping and the test harnesses send `content-type:
> application/json` — the axum JSON extractor requires it (a missing header → 415).
