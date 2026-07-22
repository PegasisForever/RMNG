# clone-daemon

`rmng-clone-daemon` runs inside each clone's headless GNOME session. It owns the desktop-facing half of RMNG:

1. Captures virtual Mutter monitors as dmabufs and ships them to the control-server over the bind-mounted Unix socket with per-monitor acknowledgement back-pressure.
2. Injects viewer input through Mutter `RemoteDesktop`.
3. Serves the clone-local desktop automation MCP on `:9004` (`RMNG_DAEMON_MCP_PORT`).
4. Bridges rich clipboard data and client-drawn cursor metadata.

The control server derives clone lifecycle from Docker liveness and passive CLIProxy token traffic; the daemon's only management surface is its clone-local desktop MCP.

## Modules

| Module | Role |
|---|---|
| `mutter.rs` | Mutter `RemoteDesktop`, `ScreenCast`, and `DisplayConfig` D-Bus setup |
| `capture.rs` / `capture_pw.rs` | GStreamer and raw-PipeWire dmabuf capture, including cursor metadata |
| `transport.rs` | `SOCK_SEQPACKET` media transport with `SCM_RIGHTS` file descriptors |
| `mcp.rs` | local desktop JSON-RPC MCP on `:9004` |
| `windows.rs` | gnome-shell `Eval` window-management tools |
| `keysym.rs` | key chord and Unicode keysym parsing |
| `clipboard.rs` | rich/lazy clipboard bridge |

## Runtime modes

With `RMNG_SOCKET` set, the daemon captures, ships frames, receives input, and serves the local MCP. Without it, it runs its capture frames-per-second self-test. `RMNG_MONITORS` provides the pre-connect monitor layout in `WxH+X+Y[*]` form; the control server replaces it with the active layout once the daemon connects.

## Capture and socket model

The selected clone keeps captured frames available for the human viewer and desktop MCP screenshots. The daemon emits only dmabufs; the control server/media crate encodes H.264 for viewers and images for tool responses. The wire schema is documented in [docs/PROTOCOL.md](../../docs/PROTOCOL.md#clone-socket-protocol-clone-daemon--control-server).

## Dependencies

`zbus`, `pipewire`/`gstreamer`, `axum`, `media`, `tokio`, `nix`, `wire`, and the clone-local desktop runtime.
