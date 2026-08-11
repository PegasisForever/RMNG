# clone-daemon

`rmng-clone-daemon` runs inside each clone's headless GNOME session, as two processes from one binary. It owns the desktop-facing half of RMNG:

1. Captures virtual Mutter monitors as dmabufs and ships them to the control-server over the bind-mounted Unix socket with per-monitor acknowledgement back-pressure.
2. Injects viewer input through Mutter `RemoteDesktop`.
3. Serves the clone-local desktop automation MCP on `:9004` (`RMNG_DAEMON_MCP_PORT`).
4. Bridges rich clipboard data and client-drawn cursor metadata.

The Mutter sessions and the virtual monitors live in the second process, the session holder (`--session-holder`, `rmng-session-holder.service`), because Mutter destroys a session when its creating D-Bus connection drops and gnome-shell remaps every window when the monitor set empties. The daemon restarts on every payload push; the holder does not, so window positions survive an update.

The control server derives clone lifecycle from Docker liveness and the agent-wrapper's activity frames; the daemon's only management surface is its clone-local desktop MCP.

## Modules

| Module | Role |
|---|---|
| `mutter.rs` | Mutter `RemoteDesktop`, `ScreenCast`, and `DisplayConfig` D-Bus setup |
| `capture.rs` / `capture_pw.rs` | GStreamer and raw-PipeWire dmabuf capture, including cursor metadata |
| `transport.rs` | `SOCK_SEQPACKET` media transport with `SCM_RIGHTS` file descriptors |
| `mcp.rs` | local desktop JSON-RPC MCP on `:9004` |
| `windows.rs` | gnome-shell `Eval` window-management tools |
| `keysym.rs` | key chord and Unicode keysym parsing |
| `clipboard.rs` | rich/lazy clipboard bridge (runs in the holder) |
| `holder.rs` | session-holder mode: sessions, monitors, layout, input, clipboard |
| `ipc.rs` | `SOCK_SEQPACKET` socket between the daemon and the holder |
| `session.rs` | the daemon's client handle on the holder |

## Runtime modes

`--session-holder` runs the holder. Otherwise, with `RMNG_SOCKET` set, the daemon captures, ships frames, relays input, and serves the local MCP; without it, it runs its capture frames-per-second self-test on a session of its own. `RMNG_MONITORS` provides the boot monitor layout in `WxH+X+Y[*]` form, used only when the holder has no remembered layout in `~/.rmng/monitors`; the control server pushes the active layout once the daemon connects.

## Capture and socket model

The selected clone keeps captured frames available for the human viewer and desktop MCP screenshots. The daemon emits only dmabufs; the control server/media crate encodes H.264 for viewers and images for tool responses. The wire schema is documented in [docs/PROTOCOL.md](../../docs/PROTOCOL.md#clone-socket-protocol-clone-daemon--control-server).

## Dependencies

`zbus`, `pipewire`/`gstreamer`, `axum`, `media`, `tokio`, `nix`, `wire`, and the clone-local desktop runtime.
