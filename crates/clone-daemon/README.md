# clone-daemon

The in-clone agent of the system: one binary, running inside each clone's headless GNOME
session, that owns everything desktop-side. It does four things:

1. **Captures** the clone's virtual monitors (Mutter ScreenCast `RecordVirtual`) as dmabufs
   and ships them to the control-server's [media](../media/README.md) ingest over a
   bind-mounted unix socket (`SCM_RIGHTS`), with per-monitor ack back-pressure.
2. **Injects input** (received from control-server) via Mutter `RemoteDesktop` — absolute +
   relative pointer, buttons, scroll, X11 keysyms, **and evdev keycodes** (physical-key
   identity for games).
3. Serves the **desktop-automation MCP** on `:9004` (`RMNG_DAEMON_MCP_PORT`): screenshot,
   click/move/scroll/key/type, and window management via gnome-shell `Eval`. The in-clone
   agent-wrapper calls it on localhost; the control-server's global MCP proxies to it. Full
   tool list: [docs/MCP.md](../../docs/MCP.md).
4. Bridges the **clipboard** (rich + lazy) and the **client-drawn cursor** (cursor-mode
   METADATA via a raw-PipeWire path), and runs the **needs-human detector** subcommands.

> This reverses the earlier "thin daemon, server-side MCP" design: keeping automation beside
> the live Mutter session it drives is simpler and avoids a second capture path. The
> control-server no longer serves desktop tools directly — it proxies here.

## Modules

| Module | Role |
|---|---|
| `mutter.rs` | zbus proxies for `org.gnome.Mutter.{RemoteDesktop,ScreenCast,DisplayConfig}`; `setup_with_cursor_mode` (RD+SC sessions, RecordVirtual N monitors) |
| `capture.rs` / `capture_pw.rs` | GStreamer dmabuf capture; raw-PipeWire path for `SPA_META_Cursor` cursor metadata GStreamer can't surface |
| `transport.rs` | `SOCK_SEQPACKET` client: ship `FrameMsg`+fds (SCM_RIGHTS), recv `ServerMsg` |
| `mcp.rs` | the HTTP JSON-RPC desktop MCP (`:9004`); shares the live `rd` + per-monitor latest dmabuf; emits cursor-warp on MCP-driven moves |
| `windows.rs` | window-mgmt tools via gnome-shell `org.gnome.Shell.Eval` (needs the shell-03 patch) |
| `keysym.rs` | key-combo / char → X11 keysym parsing (`ctrl+c`, Unicode `type`) |
| `clipboard.rs` | rich/lazy clipboard bridge over Mutter's selection API |
| `detector.rs` | `wait-for-stuck` / `report-detection` subcommands (needs-human detector) |

## CLI

- **Default (shipping mode):** with `RMNG_SOCKET` set, capture + ship + inject + serve the
  MCP. With no socket, run a capture-fps self-test. Monitors come from `RMNG_MONITORS`
  (`WxH+X+Y[*]`, comma-separated). See [docs/PROTOCOL.md](../../docs/PROTOCOL.md#clone-daemon-cli).
- **`wait-for-stuck`** — needs-human detector: screen mode (default) pulls screenshots from
  the local MCP, tiles, asks the inference vision-LLM; text mode (`--text-cmd`, e.g. a
  `tmux capture-pane` command) judges the command's text output against `--criteria` (the
  operator's semantic working/stuck definition) with a text-only completion + a
  did-the-text-change signal. Exits 0 when stuck (`--inference-url`, `--ignore-reason`,
  `--interval`, `--timeout`).
- **`report-detection`** — POST a wrong-verdict record to the control-server's
  `/api/detector-feedback` (`--kind`, `--note`, `--control`); uploads the screenshot (screen
  mode) or pane capture + criteria (text mode). The agent-wrapper spawns `wait-for-stuck`
  for monitoring. (These replace the retired `computer-use` binary.)

## Capture & socket model

- The **selected** clone (a human is viewing it on port 1) streams all monitors continuously;
  the `RecordVirtual` monitors stay held so a frame is always available. The daemon ships
  **dmabuf only** — all encoding (H.264 for the viewer, PNG for screenshots) happens in
  control-server / `media`.
- daemon → server: `Hello`, `FrameMsg`+fds, `CursorMeta`, `Layout`, `Clipboard*`.
  server → daemon: `Subscribe`, `FrameRequest`, `Ack` (releases the held PipeWire buffer),
  `Input`, `Clipboard*`. Full schema: [docs/PROTOCOL.md](../../docs/PROTOCOL.md#clone-socket-protocol-clone-daemon--control-server).
- **Cursor**: cursor-mode METADATA (no baked-in cursor); emit `CursorMeta` (position always,
  shape RGBA on change, `warp:true` after an MCP-driven move). `RMNG_EMBEDDED_CURSOR=1`
  composites the cursor into the video instead.

## Session & patches

Runs inside a headless `gnome-session` (no GDM, no g-r-d) under a `systemd --user` unit with
linger; Mutter's headless backend needs only `/dev/dri/renderD128`. The window-management MCP
tools require the **shell-03** gnome-shell patch (`org.gnome.Shell.Eval`); the clone also gets
**shell-01** (hide the screen-share pill) — both via the patched gnome-shell deb the
control-server installs ([gnome-patch](../../gnome-patch/README.md)).

## Dependencies

`zbus` (Mutter D-Bus), `pipewire`/`gstreamer` (dmabuf capture), `axum`/`reqwest` (MCP +
detector), `media`, `image`, `tokio`, `nix` (SCM_RIGHTS), `wire`.
