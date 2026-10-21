# clone-daemon

`rmng-clone-daemon` runs inside each clone's headless GNOME session, as two processes from one binary. It owns the desktop-facing half of RMNG:

1. Captures virtual Mutter monitors as dmabufs and ships them to the control-server over the bind-mounted Unix socket with per-monitor acknowledgement back-pressure.
2. Injects viewer input through Mutter `RemoteDesktop`.
3. Serves the clone-local desktop automation MCP on `:9004` (`RMNG_DAEMON_MCP_PORT`).
4. Bridges rich clipboard data and client-drawn cursor metadata.
5. Captures the desktop's sound (the default sink's monitor), encodes it to Opus, and ships the packets to the control-server, while the clone is watched.

The Mutter sessions and the virtual monitors live in the second process, the session holder (`--session-holder`, `rmng-session-holder.service`), because Mutter destroys a session when its creating D-Bus connection drops and gnome-shell remaps every window when the monitor set empties. The daemon restarts on every payload push; the holder does not, so window positions survive an update.

The control server derives clone lifecycle from Docker liveness, the agents' transcripts, and the clone's assistant chat; the daemon's only management surface is its clone-local desktop MCP.

## Modules

| Module | Role |
|---|---|
| `mutter.rs` | Mutter `RemoteDesktop`, `ScreenCast`, and `DisplayConfig` D-Bus setup |
| `capture_pw.rs` | raw-PipeWire dmabuf capture, including cursor metadata |
| `audio.rs` | desktop sound: `pipewiresrc` (sink monitor) → Opus → `DaemonMsg::Audio`, with a silence rule |
| `transport.rs` | `SOCK_SEQPACKET` media transport with `SCM_RIGHTS` file descriptors |
| `mcp.rs` | local desktop JSON-RPC MCP on `:9004` |
| `windows.rs` | gnome-shell `Eval` window-management tools |
| `keysym.rs` | key chord and Unicode keysym parsing |
| `clipboard.rs` | rich/lazy clipboard bridge (runs in the holder) |
| `holder.rs` | session-holder mode: sessions, monitors, layout, input, clipboard |
| `ipc.rs` | `SOCK_SEQPACKET` socket between the daemon and the holder |
| `session.rs` | the daemon's client handle on the holder |

## Runtime modes

`--session-holder` runs the holder. Otherwise the daemon captures, ships frames, relays input, and serves the local MCP. It needs `RMNG_SOCKET` (the media socket path) and exits with an error without it. `RMNG_MONITORS` provides the boot monitor layout in `WxH+X+Y[*]` form, used only when the holder has no remembered layout in `~/.rmng/monitors`; the control server pushes the active layout once the daemon connects.

## Capture and socket model

The daemon captures only while the control server asks for it (`Capture{active}`, sent while a viewer watches the clone), and wakes capture briefly on its own for a desktop MCP screenshot. The sound capture follows `Capture{active}` too, but a screenshot's wake does not start it. It ships dmabufs to the control server, which encodes H.264 for viewers. Screenshots are encoded to JPEG inside the clone, through the `media` crate. The wire schema is documented in [docs/PROTOCOL.md](../../docs/PROTOCOL.md#clone-socket-protocol-clone-daemon--control-server).

## Dependencies

`zbus`, `pipewire`/`gstreamer`, `axum`, `media`, `tokio`, `nix`, `wire`, and the clone-local desktop runtime.
