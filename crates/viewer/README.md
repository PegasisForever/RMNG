# viewer

The native live viewer — a **from-scratch** GTK4 client that connects to control-server
**port 1**, hardware-decodes the selected clone's H.264 monitor streams, renders them
zero-copy, and captures input back. It runs in two modes over one shared core: a **GUI**
mode and a first-class **headless** mode for testing (see
[Headless mode](#headless-mode-first-class)). This is the production successor to the old
RDP client (`../../core`/`../../gtk`/`../../headless`), and it is the one component the user
explicitly required be **written clean — no code imported from the old GTK client**
(see [Clean-room](../../README.md#clean-room)). The Phase-0 PoC (which extended the old
client) proved the approach; this crate re-implements it fresh.

## What it does

1. **Connect** to control-server port 1, send `ViewerHello` (token + capabilities),
   receive `MonitorList`.
2. **Decode** each monitor's `VideoAu` (Annex-B H.264) on a VA-API decoder → zero-copy
   `dmabuf` frame. Skip access units until the first SPS/PPS+IDR (suppresses pre-IDR
   decode noise — a Phase-0 polish item).
3. **Render** via GTK4: import each decoded dmabuf as a `Gdk::DmabufTexture`, paint with a
   frame-clock tick callback (latest-wins, no display FIFO). One surface/area per monitor.
4. **Cursor**: the video has no baked-in cursor (server captures METADATA). The **native OS
   cursor is shown** over the video and **takes the remote cursor's shape** — each `CursorMeta`
   `CursorShape` (BGRA bitmap + hotspot) becomes a `gdk::Cursor` set on the video widget, so the
   operator's own pointer turns into the I-beam / hand / resize cursor the remote shows (zero-lag
   movement; shape updated on change). The **synthetic overlay** is drawn on top **only while the
   remote agent drives the pointer** — on a server-sent **`warp:true`** update (an MCP-driven
   move) it's drawn at the agent's target for ~1 s (per-monitor, refreshed by each warp) so the
   operator sees where the agent moved, while their own native cursor keeps showing. A warp also
   **suppresses local pointer motion for ~0.5 s** so the operator's mouse doesn't fight the agent.
5. **Capture input** → `InputMsg` to the server: absolute pointer (per-monitor scaled),
   buttons, scroll, and the keyboard. Normal keys go as **X11 keysyms** (GTK `keyval`, no
   DOM→keysym keymap); physical keys also go as **evdev keycodes** (`hardware_keycode − 8`)
   for games. Pointer motion is coalesced (latest-wins, ~120 Hz).
6. **Pointer-lock / relative mouse** (games, e.g. Minecraft mouse-look): toggle with
   **Ctrl+Alt+G** (Ctrl+Alt+P releases / unsticks all input). While engaged it **hides the
   native cursor** (the only mode that does) and sends unaccelerated `pointer_relative` deltas
   (raw `wayland-client` + `gdk4-wayland` pointer-constraints). Opt out with `RMNG_NO_POINTER_LOCK`.
7. **Window chrome**: a HeaderBar per monitor window, **F11** fullscreen toggle, and an
   in-GUI **FPS** readout (paintable invalidate count, 1 s timer).
8. **Clipboard**: bridge the GTK clipboard to the server's broker (rich + lazy) — offer on
   local copy, request the chosen MIME on paste, move bytes via `ClipboardData`.
9. **Reconnect**: on drop, reconnect and `RequestKeyframe`; the server forces a fresh IDR.

## Headless mode (first-class)

The viewer ships **two front-ends over one shared core** — the GUI above, and a **headless**
mode for testing with **no window and no display server**. This is first-class, not an
afterthought: the transport (port-1 connect), the H.264 decode, the input/clipboard
protocol, and the reconnect logic are all in a backend-agnostic core; the GUI and headless
modes differ only in their *render sink* and *input source*.

Headless mode (`viewer --headless …`):
- **Connects + decodes** exactly like the GUI, but renders to memory instead of a window.
- **Assertions / capture**: dump any monitor's latest frame to PNG, checksum it, and report
  decode **fps / latency / IDR timing** — so a test can assert "the picture is live and
  matches the source."
- **Scripted input**: drive `ViewerInput` from a script/stdin/flags (move, click, type,
  scroll, keysyms) and exercise the clipboard — so a test can assert input round-trips
  without a human.
- **CI-friendly**: runs over SSH / in a container with no `WAYLAND_DISPLAY`; the obvious way
  to write end-to-end tests (spin up control-server + a clone or stub → run the headless
  viewer → assert frames + input). It is how *this crate itself* is tested.

(The old client's separate `../../headless` binary motivates this, but here it's a mode of
the one viewer, sharing all non-render code — not a second implementation.)

## Design notes carried from the PoC (validated)

- **Zero-copy throughout**: NAL → VA-API surface → dmabuf → `Gdk::DmabufTexture`; no CPU
  copies. Intel iGPU decode validated against AMD-encoded streams.
- **Pace on the frame clock**, blit the newest frame; don't queue. The PoC's large-window
  60fps cap was **decoder surface starvation** — size the decoder surface pool with
  headroom so decode can run ahead of present.
- **Multi-monitor** = N streams off one connection, each `VideoAu` tagged `monitor_id`;
  lay monitors out per `MonitorList` geometry. (The PoC was single-monitor; multi-monitor
  is the main net-new work here.)
- A manual window-drag is gated by the **pointer event rate**, not the pipeline — expected,
  not a bug (Phase-0 perf finding).

## Relationship to host selection

The viewer shows whatever clone is **`selected`** in `ControlState`. Selection itself
happens in the browser management UI (port 2); the server re-points port 1 at the new
clone and the viewer just sees the monitor set change. The viewer may optionally subscribe
to `/events` (port 2) to display which clone it is showing, but it does not drive selection.

## Dependencies

`gtk4`/`gdk4` + `gstreamer` for the GUI mode — render is a **GStreamer `vah264dec ! glupload !
gtk4paintablesink`** pipeline (zero-copy VA-API decode into a GTK paintable); pointer-lock
uses raw `wayland-client`/`wayland-protocols` + `gdk4-wayland`. `tokio` (port-1 socket),
`wire`. The decode/transport core is GTK-free so **headless mode needs no display server**. No
dependency on `../../core`, `../../gtk`, `../../headless`, or `../../shared`.

## Tests

Most tests run the **headless mode** (no display) against control-server + a clone or stub:
- Connects, negotiates `MonitorList`, paints within ~1 IDR interval of connect.
- Decoded output matches the source (checksum a frame end-to-end).
- Scripted input round-trips: keys/mouse/scroll move the real desktop; modifiers + non-US
  layout work via keysyms (the case the browser needed noVNC for).
- Clipboard round-trips both directions (rich MIME, lazy fetch).
- Reconnect recovers the picture via a forced IDR.
- Multi-monitor clone: all monitors decode; input routes to the right monitor.

> Settled during implementation: decode is GStreamer `vah264dec` → `gtk4paintablesink`
> (Intel can't export VA dmabuf via GStreamer, so the GL upload path is used); multi-monitor
> is **one window per monitor**.
