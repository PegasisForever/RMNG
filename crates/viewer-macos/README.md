# viewer-macos

The **native macOS client**: AppKit windows, VideoToolbox hardware H.264 decode, and a Metal
render path. No GTK, no GStreamer — the binary links only system frameworks, so it needs no
Homebrew at runtime and ships as a self-contained `.app`.

```sh
cargo build -p viewer-macos --release      # → target/release/rmng-viewer-macos
```

## Why it exists

The GTK viewer ([`crates/viewer`](../viewer/README.md)) runs on macOS, but GDK's macOS backend
re-derives pointer state itself (tracking areas, an `inMove` flag, its own surface hit-test and
y-flip) and drops motion whenever that machinery disagrees with reality. The symptom that forced
the rewrite: **in fullscreen, moving the pointer into the top ~50 px stopped motion reaching the
remote, and it stayed stuck until you clicked lower down.** Three input paths had already been
routed around GDK (keyboard, pointer lock, titlebar); absolute pointer motion was the last one
still going through it.

Here the app owns an `NSView` subclass, so `mouseMoved:` and `keyDown:` arrive **directly from
AppKit**. There is no layer left to lose track of the pointer, and `keyCode` is the true Carbon
virtual key with no `interpretKeyEvents:` text-input mangling.

## Shape

| Module | What |
|---|---|
| [`net.rs`](src/net.rs) | port-1 reconnect loop + tag dispatch (video / clipboard / cursor / view spec / chroma / forwards / terminal) |
| [`decoder.rs`](src/decoder.rs) | one `VTDecompressionSession` per monitor; Annex-B → AVCC → `CVPixelBuffer` (IOSurface-backed) |
| [`render.rs`](src/render.rs) | `CVMetalTextureCache` → Y (R8) + CbCr (RG8) textures → BT.709-limited NV12→RGB shader, letterboxed via the Metal viewport |
| [`window.rs`](src/window.rs) | the `NSView` subclass (direct mouse/keyboard input, letterbox inverse) + its `NSWindow` |
| [`app.rs`](src/app.rs) | `NSApplication`, the net-thread → main-thread wake hop, window reconcile from `ViewSpec`, draw-on-frame |
| [`shared.rs`](src/shared.rs) | state shared net-thread ⇄ main thread, and the viewer→server framing |

Toolkit-free logic (config, auto pointer-lock policy, port-forward listeners, the kVK→evdev
table) is shared with the GTK viewer via [`viewer-core`](../viewer-core).

## Threading

The net thread reads port 1 and decodes; VideoToolbox hands back an IOSurface-backed
`CVPixelBuffer`, which is latched into a per-monitor slot (latest-wins) and a coalesced wake is
dispatched to the main queue. The main thread owns every AppKit and Metal object and does the
texture creation, render, and input. `WinCtx` is `Rc` (main-thread only); only `Shared` crosses
threads.

## Status

Working: connect + reconnect, chroma handshake, hardware decode, Metal render (4:2:0), absolute
pointer, buttons, scroll, physical keyboard with the Cmd↔Ctrl swap, F11 fullscreen, window
reconcile from the `ViewSpec`, `--headless` fps mode.

Not yet ported from the GTK viewer: the 4:4:4 AVC444 Metal unpack (`--unpack-validate` currently
reports it as pending; a 4:4:4 stream renders from its main view meanwhile), the remote-cursor
shape + agent-warp overlay, clipboard, pointer lock / auto-lock, the settings dialog, and the
headless-clone terminal tabs.
