# viewer

The native live viewer — a **from-scratch** GTK4 client that connects to control-server
**port 1**, hardware-decodes the selected clone's H.264 monitor streams, renders them
zero-copy, and captures input back. It runs in two modes over one shared core: a **GUI**
mode and a first-class **headless** mode for testing (see
[Headless mode](#headless-mode-first-class)). This is the production successor to the old
RDP client (`../../core`/`../../gtk`/`../../headless`), and it is the one component the user
explicitly required be **written clean — no code imported from the old GTK client**
(see [Clean-room](../../docs/DEVELOPMENT.md#clean-room)). The Phase-0 PoC (which extended the old
client) proved the approach; this crate re-implements it fresh.

## What it does

1. **Connect** to control-server port 1, send `ViewerHello` (token + capabilities),
   receive `MonitorList`.
2. **Decode** each monitor's `VideoAu` (Annex-B H.264) on a VA-API decoder → zero-copy
   `dmabuf` frame. Skip access units until the first SPS/PPS+IDR (suppresses pre-IDR
   decode noise — a Phase-0 polish item).
3. **Render** via GTK4: import each decoded dmabuf as a `Gdk::DmabufTexture`, paint with a
   frame-clock tick callback (latest-wins, no display FIFO). One surface/area per monitor.
   The decoded frames are **retagged `2:3:7:1`** first (limited range, BT.709 matrix, sRGB
   transfer, BT.709 primaries). See [Colour](#colour).
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

## Colour

A clone's desktop is sRGB, and the server's encoder only applies a matrix + range change to it
(`media::encode`), so the samples that arrive are sRGB-encoded, limited-range, BT.709-matrix
YUV. Nothing in the bitstream says so: `vah264enc` writes no VUI colour description, so the
decoded frames arrive with no colour description at all.

Left that way, GStreamer fills in its own default, which names the **BT.709 transfer**, and GTK
then converts that to sRGB when it composites the frame. The conversion lifts everything below
white (16 renders as 32, 128 as 140, 224 as 227), and the desktop reads washed out beside the
same pixels shown natively. The measured error is up to 16 of 255.

So both decode paths state the truth instead of inheriting a guess:

- 4:2:0: `retag_colorimetry` rewrites the decoder's sticky caps event to `2:3:7:1`. A
  capsfilter cannot: the memory feature on those caps differs per platform, and a decoder that
  named its own colour description would fail to negotiate against a fixed one.
- 4:4:4: `rmngavc444unpack` declares `1:1:7:1` (full range, RGB, sRGB) on its RGBA output,
  which is what the shader writes, rather than passing the packed stream's YUV description on.

With the tag in place a patch chart survives capture → encode → decode → GTK within 1 of 255.

## Per-OS backends

One toolkit-free core, three platform backends. The transport, the wire protocol and the AVC444
packing are identical; only these pieces differ. The numbered list above describes the Linux
column.

| | Linux | macOS (Apple Silicon) | Windows |
| --- | --- | --- | --- |
| H.264 decode | `vah264dec` (VA-API) | `vtdec_hw` (VideoToolbox) | `d3d11h264dec` (D3D11VA), falling back to `avdec_h264` / `openh264dec` — **chosen at runtime**, see below |
| GL import | `glupload` → 2D `GLMemory` | `vtdec_hw` emits IOSurface-backed `GLMemory` directly; `glupload` drops out | `d3d11download ! glupload` → 2D `GLMemory` (there is no D3D11↔GL interop in GStreamer, so the frame makes one sysmem round trip) |
| Texture target | `2D` | `rectangle` (Apple's `CGLTexImageIOSurface2D` accepts only `GL_TEXTURE_RECTANGLE`) | `2D` |
| 4:2:0 sink path | `glupload ! gtk4paintablesink` | `vtdec_hw ! glcolorconvert ! gtk4paintablesink` (the sink takes RGBA 2D only) | `glupload ! glcolorconvert ! gtk4paintablesink` (same reason as macOS) |
| 4:4:4 unpack | `rmngavc444unpack`, `sampler2D`, `#version 300 es` | `rmngavc444unpack`, `sampler2DRect`, desktop GLSL (Apple has no `ARB_ES3_compatibility`) | `rmngavc444unpack`, `sampler2D` — same shader as Linux |
| GL platform | EGL | CGL (desktop GL 4.1 over Metal) | WGL |
| Keyboard | GTK `EventControllerKey`, `evdev = hardware_keycode − 8` | raw `NSEvent` local monitor + `kvk_evdev` table ([`keyboard_macos.rs`](src/keyboard_macos.rs)); GDK-swallowed keys still come via GTK. Cmd↔Ctrl swapped by default | GTK `EventControllerKey` + `vk_evdev` table ([`vk_evdev.rs`](src/vk_evdev.rs)): the VK is inverted to a set-1 scancode first, because a VK is **not** a physical key |
| Pointer lock | `zwp_pointer_constraints` + `zwp_relative_pointer`, unaccelerated deltas ([`pointer_lock.rs`](src/pointer_lock.rs)) | `CGAssociateMouseAndMouseCursorPosition` + `NSEvent` deltas, which are OS-**accelerated** ([`pointer_lock_macos.rs`](src/pointer_lock_macos.rs)) | `ClipCursor` + Raw Input `WM_INPUT`, unaccelerated deltas ([`pointer_lock_win.rs`](src/pointer_lock_win.rs)) |
| Titlebar | GTK `HeaderBar` + FPS readout | real `NSWindow` titlebar + `NSButton` accessories ([`native_titlebar.rs`](src/native_titlebar.rs)); no FPS readout | GTK `HeaderBar` + FPS readout (same as Linux) |
| GSK renderer | `gl` pinned (stale-texture workaround) | `ngl` — the legacy `gl` renderer was removed in GTK 4.18, so no pin is available | `ngl`, same as macOS |
| System shortcuts | `inhibit_system_shortcuts` — Super and Alt+Tab reach the remote | not inhibited (GDK has no macOS implementation) | not inhibited; see [Windows limitations](#windows-limitations) |

Build and run instructions: [DEVELOPMENT.md § macOS](../../docs/DEVELOPMENT.md#macos),
[DEVELOPMENT.md § Windows](../../docs/DEVELOPMENT.md#windows).

### Why Windows picks its decoder at runtime

Linux and macOS each hardcode one decode element because each has exactly one answer: VA-API on
the known deploy GPU, VideoToolbox on every Mac. Windows has neither guarantee — `d3d11h264dec`
registers only when the installed GStreamer ships the D3D11 plugin *and* the GPU advertises an
H.264 decode profile, which a VM, a remote session, or a trimmed GStreamer build will not. A
hardcoded element would turn each of those into "cannot build the decode pipeline" and a blank
window, so `win_decoder()` in [`main.rs`](src/main.rs) takes the first of `d3d11h264dec`,
`avdec_h264`, `openh264dec` that actually registered and logs which one it got.

The two software arms insert `videoconvert ! video/x-raw,format=NV12`, because libav and
OpenH264 emit I420. That is safe for AVC444: the invariant `rmngavc444unpack` depends on is that
nothing **resamples** the packed chroma before it, and I420 → NV12 is a pure re-layout of the
same 4:2:0 samples. A converter asked for 4:4:4 or RGB would upsample and destroy the auxiliary
view — which is why none appears anywhere ahead of the unpacker on any platform.

### Windows limitations

- **Physical-key fidelity has one gap.** GDK hands us a virtual key, and Windows uses the same
  `VK_RETURN` for both `Enter` keys (they differ only by the extended bit in `lParam`, which GDK
  does not forward), so keypad `Enter` arrives as `KEY_ENTER` rather than `KEY_KPENTER`.
  Everything else — including the whole navigation cluster, both `Ctrl`/`Shift`/`Alt`/`Meta`
  pairs, and non-US layouts — resolves to the correct physical position. The fix, if it is ever
  needed, is to read `RAWKEYBOARD.MakeCode` from the Raw Input stream `pointer_lock_win` already
  runs a message window for, and feed `vk_evdev::scancode_to_evdev` directly.
- **System shortcuts are not inhibited.** `Super` opens the local Start menu and `Alt+Tab`
  switches local windows instead of reaching the remote. GDK implements
  `inhibit_system_shortcuts` for Wayland only, so this matches the macOS backend's behaviour
  rather than Linux's. Capturing them would need a `WH_KEYBOARD_LL` hook.
- **The frame makes one sysmem round trip.** GStreamer has no D3D11↔GL interop path, so a
  hardware-decoded frame goes VRAM → sysmem (`d3d11download`) → VRAM (`glupload`) rather than
  staying resident as it does on Linux and macOS. Everything after the upload — including the
  AVC444 reconstruction — is still GPU-side.

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
- **Clipboard mirror**: receive-only, since there is no local clipboard to own. Every offer
  is fetched with the same MIME choice the GUI makes and logged under the `clip` target, so a
  test can assert what a copy inside a clone put on the wire. `RMNG_CLIP_ECHO=1` logs the
  first 120 characters of each text payload.
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

## Relationship to clone selection

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
