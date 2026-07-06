# MCP reference — the two surviving desktop-automation surfaces

RMNG exposes **two** MCP servers, both speaking **JSON-RPC 2.0 over HTTP POST** at
path `/`. One is on the control-server; one is in each clone-daemon.

> **Migration:** the former **global MCP** (`:9003`) — the operator/fleet-facing desktop
> proxy — has been **removed**. Its 14 desktop/window tools now live in the **`rmng desktop`
> CLI** ([CLI.md](CLI.md#rmng-desktop-clone-verb-alias-dt)). External configs that registered
> `:9003` as an MCP server must switch to the `rmng desktop` CLI; there are no in-repo
> consumers.

| Server | Where | Default port | Scope | Source |
|---|---|---|---|---|
| **per-clone MCP** | control-server | `9002` (`listen.clone_mcp`) | `set_state` only; clone resolved from the **`x-rmng-clone` header** | [control-server/src/mcp.rs](../crates/control-server/src/mcp.rs) |
| **daemon MCP** | each clone-daemon | `9004` (`RMNG_DAEMON_MCP_PORT`) | the **full desktop-automation surface** (input/screenshot/window-mgmt) | [clone-daemon/src/mcp.rs](../crates/clone-daemon/src/mcp.rs), [windows.rs](../crates/clone-daemon/src/windows.rs) |

MCP is for tools whose results belong in model context (screenshots), not orchestration:
**fleet management** (hosts, clone/delete, images, accounts, operations) and **operator/fleet
desktop control** both live in the **`rmng` CLI** over the port-2 web API — see
[CLI.md](CLI.md) — and **host-agent chat** lives on the web API too (`/api/chat/:id` — see
[API.md](API.md#per-host-agent-chat)).

**Who calls what:**
- The **in-clone agent-wrapper** calls its clone's **daemon MCP** directly on
  `http://127.0.0.1:9004` for desktop actions, and the control-server's **per-clone MCP**
  (`:9002`) for `set_state`.
- **Operators / fleet agents** drive any clone's desktop through the **`rmng desktop` CLI**
  ([CLI.md](CLI.md#rmng-desktop-clone-verb-alias-dt)); the control-server proxies each verb to that
  clone's daemon MCP at `http://{clone}:9004`. Everything else goes through the `rmng` CLI /
  web API.

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
`x-rmng-clone` header is missing or names no known host.

---

## Daemon MCP (clone-daemon `:9004`)

Runs inside each clone, sharing the daemon's live Mutter `RemoteDesktop` session (input),
its per-monitor latest-dmabuf (screenshots via `media::screenshot_jpeg`), and gnome-shell
`org.gnome.Shell.Eval` (window management). No `clone` argument — it *is* one clone.

### Input & capture tools

| Tool | Args | Behaviour |
|---|---|---|
| `list_monitors` | — | `[{id,width,height}]` |
| `screenshot` | `monitor?`=0 | JPEG of the monitor's latest captured frame (image). Errors if no frame buffered yet |
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
# Daemon MCP (on the clone): list every tool
curl -s localhost:9004/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq

# Daemon MCP (on the clone): screenshot monitor 0
curl -s localhost:9004/ -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,
  "method":"tools/call","params":{"name":"screenshot","arguments":{"monitor":0}}}'

# To drive a clone's desktop from an operator machine, use the CLI instead:
#   rmng desktop rmng-e2e screenshot   (proxied to the clone's daemon MCP — see CLI.md)

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
