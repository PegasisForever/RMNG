# MCP reference — clone-local desktop automation

RMNG exposes one MCP server: each clone daemon serves **JSON-RPC 2.0 over HTTP POST** at `/` on `:9004` (`RMNG_DAEMON_MCP_PORT`). It owns the clone's live Mutter desktop session.

Clone lifecycle state is owned directly by the control server from Docker liveness and passive CLIProxy token activity. Fleet and operator desktop control use the [`rmng` CLI](CLI.md), which proxies desktop calls through the web API.

| Server | Where | Default port | Scope | Source |
|---|---|---:|---|---|
| **daemon MCP** | each clone daemon | `9004` | desktop input, capture, and window management | [clone-daemon/src/mcp.rs](../crates/clone-daemon/src/mcp.rs), [windows.rs](../crates/clone-daemon/src/windows.rs) |

The agent wrapper calls `http://127.0.0.1:9004` directly. Codex receives the same `desktop` MCP entry in its managed configuration. Operators use `rmng desktop <clone> <verb>`; the control server forwards the request to that clone's daemon MCP.

## JSON-RPC envelope

```json
{ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
  "params": { "name": "<tool>", "arguments": { } } }
```

Success: `{ "jsonrpc":"2.0", "id":1, "result": { "content": [ … ] } }`.

Core methods are `initialize`, `ping`, `tools/list`, and `tools/call`. Tool content items are text or base64 PNG images. Most desktop input tools return a post-action screenshot after a short settle.

## Daemon MCP (`:9004`)

The daemon shares its live Mutter `RemoteDesktop` session, latest per-monitor dmabuf screenshot, and gnome-shell `org.gnome.Shell.Eval` window-management bridge.

### Input and capture tools

| Tool | Args | Behaviour |
|---|---|---|
| `list_monitors` | — | `[{id,width,height}]` |
| `screenshot` | `monitor?` = 0 | latest monitor frame as an image |
| `mouse_move` | `x`, `y`, `monitor?` = 0 | bounds-clamped pointer movement, then screenshot |
| `left_click` / `right_click` / `middle_click` | `x?`, `y?`, `monitor?` = 0 | optional move, click, settle, screenshot |
| `left_double_click` | `x?`, `y?`, `monitor?` = 0 | optional move, double click, settle, screenshot |
| `scroll` | `amount`, `x?`, `y?`, `monitor?` = 0 | bounded discrete scroll |
| `key` | `keys` | a `+`-joined key chord such as `ctrl+c` |
| `type` | `text` | Unicode text entry |

### Window-management tools

These require the shell `Eval` patch.

| Tool | Args | Returns |
|---|---|---|
| `list_windows` | — | current window metadata |
| `move_window` | `id`, `monitor?`, `mode?` | moved/maximized/centered window metadata |

## Examples

```sh
# On a clone: list available desktop tools
curl -s localhost:9004/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq

# On a clone: capture monitor 0
curl -s localhost:9004/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"screenshot","arguments":{"monitor":0}}}'

# From an operator machine: drive a clone through the control-server web proxy
rmng desktop rmng-e2e screenshot
```
