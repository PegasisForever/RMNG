# Browser viewer: scope

This document decides what the new browser viewer includes. It lists each feature that Selkies has and each feature
of the current native viewer. For each one, it gives a proposal, an effort label and the decision. All rows are decided.

## Already decided

- The browser receives video over a **WebSocket** and decodes it with **WebCodecs**. There is no WebRTC.
- RMNG keeps its own capture (clone-daemon, PipeWire DMA-BUF), its own encode (`crates/media`, VA-API H.264) and its
  own muxing (`control-server/src/mediaplane.rs`).
- The viewer becomes part of the RMNG web dashboard on port 9000.
- The switch happens in one step. There is no period in which the native viewer and the browser viewer both work.
  The native viewers (`crates/viewer`, `crates/viewer-macos`) and port 9001 are deleted in the same change.
- Audio stays out. RMNG removed it in `59bb9cc`.
- Chrome is the only target browser (Q2).
- No 4:4:4 colour. The 4:4:4 code is deleted (Q4, X4).
- Monitor sizes come from the fixed layout presets (Q5).
- Selkies is a source of ideas. We write our own code. We copy a Selkies file only when this document says so (see
  **How**).

## Questions (answered)

Each row that depends on one of these answers names its question ID.

| ID | Question | Why it matters | Answer |
|---|---|---|---|
| Q1 | How do we get HTTPS? Options: `tailscale cert` with rustls in control-server, or `tailscale serve` in front of port 9000. | WebCodecs, the async clipboard and Keyboard Lock work only on HTTPS pages (or `localhost`). `http://<tailscale-ip>:9000` does not work. | Not our concern. Setting up Tailscale is the user's job. Show a warning when the page is not HTTPS. |
| Q2 | Which browsers must work fully? Chrome only, or also Firefox and Safari? | Window placement on screens (M2) and Keyboard Lock (I4) work only in Chrome. Safari needs extra decoder work (C4). | Chrome only. |
| Q3 | Port forwarding: drop it, use `ssh -L` through the bastion, or move it into an `rmng forward` CLI command? | A browser cannot open local ports. | Drop it. Give an `ssh` command that the user runs to get port forwarding, like the existing "Copy SSH command". |
| Q4 | Keep 4:4:4 colour in the browser? | It needs a WebGL shader and probably a CPU copy of each frame. A prototype must show the cost first (C7). | No. |
| Q5 | Keep fixed layout presets, or resize the remote monitor to the browser window like Selkies does? | Changes how monitors are created and scaled (M4, M6). | Fixed layout presets. |
| Q6 | Upload files by dropping them on the viewer? | RMNG already has the SMB share and `/shared` (O1). | Yes. |

## Questions from the review (answered)

| ID | Question | Proposal | Answer |
|---|---|---|---|
| A1 | You removed T5 and T6. What must happen when the browser cannot accept frames as fast as the server sends them? | Option (b), effort S. (a) Keep today's rule: after 128 queued messages the server closes the connection; the browser reconnects and gets a keyframe. The picture stops until the browser has reconnected. (b) The server drops frames for that browser only, then sends it a keyframe. No acknowledgement protocol. The picture stops only for a few frames. | (a) Keep today's rule. The browser reconnects by itself (C1). |
| A2 | Port-forward command: build it from the saved per-clone rules in the existing port-forward dialog, or remove the saved rules? | Keep the saved rules. "Copy port-forward command" gives one `ssh -J rmng@<host>:2222 -N -L <local>:localhost:<remote> … rmng@<clone>` with all enabled rules of that clone. Remove the live status (listening, error), because the server cannot see the user's `ssh`. | No. Remove the saved forward rules. RMNG stores no forward state. The user types a local port and a clone port, RMNG shows the `ssh` command, and stores nothing. |
| A3 | Where does a dropped file go in the clone? | `~/Downloads`. | `~/` (the clone user's home folder). |
| A4 | Also download files from the clone to the browser? | No. Upload only. Download is possible through the SMB share. | No. |
| A5 | Where does the viewer open? | In separate windows, one per monitor, opened from the dashboard. The dashboard stays in its own tab. This is the same as today with the native viewer. | Yes. |
| A6 | Rows where the Decision column is empty: do you accept the proposal for all of them? | Yes. | Yes. |

## Legend

**Proposed**
- **Yes**: include in the switch.
- **Later**: not needed for the switch. Add when a need appears.
- **No**: do not build.
- **Ask**: you decide. See the question ID.

**Effort**: one engineer working with coding agents, tests included.
- **S**: one small module, under ~300 lines. Less than 1 day.
- **M**: one feature across server and client. 1–3 days.
- **L**: hard, uncertain, or browser-specific work that needs a prototype. 1–2 weeks.
- **XL**: a full subsystem. More than 2 weeks.
- **—**: no work, because we keep the existing code or do not build the feature.

**How**
- **R**: write our own code from the Selkies idea.
- **P**: copy a Selkies file. The file stays MPL-2.0 and keeps its license header. The rest of RMNG stays MIT.
- **D**: use a third-party library.
- **K**: keep or move existing RMNG code.
- **X**: delete RMNG code.

---

## T. Transport and server (control-server)

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| T1 | WebSocket route on port 9000, same origin as the dashboard | Yes | S | R | Turn on axum's `ws` feature. Replaces TCP port 9001. | Yes |
| T2 | Binary video message: monitor id, frame id, keyframe flag, server timestamp, H.264 frame | Yes | S | R | Today: `[0][monitor_id][len][frame]`. Selkies uses a 12-byte header. | Yes |
| T3 | Control messages as JSON text messages, with the existing Rust types (`InputMsg`, `ClipboardMsg`, `CursorMeta`, ViewSpec, mode) | Yes | S | K | ts-rs already exports these types to the frontend. | Yes |
| T4 | Client asks for a keyframe (at start, after a decode error) | Yes | S | R | `force_idr()` already exists. Today the viewer cannot ask. | Yes |
| T5 | Frame acknowledgements from the client, so the server measures round-trip time and queue delay | No | M | R | Selkies sends an acknowledgement every 50 ms. | No |
| T6 | Backpressure: when a client falls too far behind, stop sending changed-only frames, then send a keyframe | No | M | R | A1: keep today's rule. After 128 queued messages the server closes the connection, and the browser reconnects (C1). | No (A1) |
| T7 | Close a socket that accepts no data for 10 s | Yes | S | R |  | Yes |
| T8 | Read the kernel send-queue size as an extra "behind" signal | Later | S | R |  | Later |
| T9 | Client asks for a lower frame rate when its decoder is too slow | Later | M | R | Selkies `decode-pace.js`. | Later |
| T10 | Bitrate cap (CBR or VBR) instead of fixed quality (CQP) | Later | M | R | Needed only off the LAN. `vah264enc` supports CBR and VBR. | Later |
| T11 | Congestion control: lower the bitrate when queue delay grows | Later | L | R | Needs T10. | Later |
| T12 | Repair a lost frame without a keyframe (reference invalidation) | No | XL | — | Needs long-term-reference control in the encoder. VA-API on AMD probably cannot do it. | No |
| T13 | Keyframes only on demand (no fixed interval) | Later | S | R | Needs T4 and T6. Today a keyframe comes every 30 frames. Saves bitrate on a still screen. | Later |
| T14 | Paint-over: send a still screen again at higher quality | Later | M | R | Sharper text at a lower average bitrate. Useful only with T10. | Later |
| T15 | Gzip for control messages | No | — | — |  | No |
| T16 | Send encoder facts (hardware, zero-copy, GPU) to the client for the stats view | Later | S | R |  | Later |
| T17 | Warning when the page is not HTTPS | Yes | S | R | Q1. Without HTTPS the viewer cannot start, so the viewer area shows the warning instead of video. The dashboard keeps working over HTTP. Use `wss://` when the page is HTTPS. | Yes (Q1) |
| T18 | Authentication: passwords, tokens, view-only access, origin checks | No | — | — | The RMNG scope rules say no authentication. | No |
| T19 | Reverse-proxy path prefix, listen on a unix socket | No | — | — |  | No |
| T20 | Run a command when a client connects or disconnects | No | — | — |  | No |
| T21 | Recording tap, audit webhook, Prometheus metrics, statistics CSV files | No | — | — |  | No |
| T22 | Command API and the Computer-Use HTTP server | No | — | K | RMNG already has its web API and the MCP on port 9004. | No |
| T23 | WebRTC transport, packet pacer, STUN/TURN, ICE | No | XL | — | Decided. | No |


## E. Capture and encode (pixelflux in Selkies)

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| E1 | Capture from Mutter through PipeWire DMA-BUF | Yes | — | K | Decided. | Yes |
| E2 | VA-API H.264 encode in control-server, one encoder per monitor, shared by all viewers | Yes | — | K | Decided. | Yes |
| E3 | Other codecs: H.265, VP8, VP9, AV1 | No | L | — | No gain on the LAN. The W6800 cannot encode AV1. | No |
| E4 | Striped H.264 and JPEG, for browsers without WebCodecs | No | — | — | All current Chrome, Firefox and Safari versions have WebCodecs. | No |
| E5 | Software encoders (x264 and others) | No | — | — |  | No |
| E6 | True H.264 4:4:4 profile | No | — | — | AMD VA-API cannot encode it. Our packed 4:4:4 is in C7. | No |
| E7 | 10-bit colour | No | — | — |  | No |
| E8 | Cursor drawn into the video on the server | No | — | — | RMNG sends the cursor separately. | No |
| E9 | Watermark | No | — | — |  | No |
| E10 | Automatic GPU selection | No | — | K |  | No |

## C. Browser: decode and display

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| C1 | WebSocket and decoder in a dedicated worker, away from the page thread. Reconnect by itself after the connection closes, then ask for a keyframe (T4). | Yes | M | R | A1 depends on the reconnect. | Yes |
| C2 | WebCodecs `VideoDecoder`: codec string read from the SPS, low-delay mode | Yes | S | R | Selkies `wire-codecs.js` can be copied (P), but we only need H.264. | Yes |
| C3 | Give the decoder the colour space explicitly (BT.709 limited) | Yes | S | R | Browsers ignore the colour data in the stream. | Yes |
| C4 | Safari: convert Annex B to avcC, keep chunk timestamps strictly increasing | No | M | R | Q2: Chrome only. | No (Q2) |
| C5 | Decoder recovery: on error, reset, drop frames until a keyframe and ask for one (T4). Use the software decoder after repeated crashes. | Yes | S | R |  | Yes |
| C6 | Show 4:2:0 frames: decoded frame to `<video>` through `VideoTrackGenerator` (no copy), canvas as fallback | Yes | M | R |  | Yes |
| C7 | Show packed 4:4:4 (AVC444): port the `glunpack.rs` shader to WebGL2 | No | L | R | Q4. See X4 for the code to delete. | No (Q4) |
| C8 | Measure the screen refresh rate and match the stream frame rate to it | Later | S | P | Selkies `display-refresh.js`. | Later |
| C9 | Stats view: frame rate, end-to-end delay, decode time, hardware or software decoder | No | M | R | Removed after round 9 at the operator's request; `rmng-fps` measures the frame rate. | No |
| C10 | "Poor connection" indicator | Later | S | R |  | Later |
| C11 | Audio playback, microphone, webcam | No | — | — | Audio was removed in `59bb9cc`. | No |

## I. Input

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| I1 | Keyboard: `KeyboardEvent.code` to Linux keycode (`key_code`) | Yes | M | R | Selkies sends X11 keysyms through the noVNC tables. We do not need them. The remote keyboard layout applies, as in the native viewer. | Yes |
| I2 | Track held keys. Release all of them when the window loses focus or the tab is hidden. | Yes | S | R | Prevents stuck keys. | Yes |
| I3 | macOS Command key: same mapping as the native macOS viewer | Yes | S | K | As the native macOS viewer did by default: Cmd sends Ctrl, Ctrl sends Super, and Caps Lock is sent as a tap. Fixed after round 9; the first version sent Cmd as Super. | Yes |
| I4 | Keyboard Lock in fullscreen, so Chrome passes Ctrl+W, Alt+Tab and Super to the clone | Yes | S | R | Chrome only, which is the only target (Q2). | Yes |
| I5 | IME composition (Chinese, Japanese, Korean input) | No | L | — | The native viewer has no IME either. | No |
| I6 | Keyboard layout detection | No | — | — | Not needed with Linux keycodes. | No |
| I7 | Absolute pointer and buttons (with back and forward). Map page pixels to monitor pixels, with device pixel ratio and letterbox. | Yes | S | R |  | Yes |
| I8 | Scroll: wheel steps and smooth trackpad scroll (`axis_continuous`) | Yes | S | R |  | Yes |
| I9 | Combine pointer moves, use `pointerrawupdate` | Yes | S | R | Built with `pointermove`, not `pointerrawupdate`: Chrome already sends at most one `pointermove` per frame, so moves need no other merging. | Yes |
| I10 | Pointer lock with raw (unaccelerated) motion, sent as `pointer_relative` | Yes | S | R |  | Yes |
| I11 | Lock the pointer automatically when the remote cursor is hidden for 180 ms (`auto_lock.rs`) | Yes | M | K | Chrome lets a focused page lock the pointer without a click, except after the user leaves the lock with Esc: then the next lock needs a click. Check this in the prototype. | Yes |
| I12 | Shortcuts: Ctrl+Alt+G locks the pointer, Ctrl+Alt+P releases it | Yes | S | K |  | Yes |
| I13 | Agent pointer moves: ignore local motion for 0.5 s, draw the cursor for 1 s | Yes | S | K |  | Yes |
| I14 | Touch, virtual trackpad, on-screen keyboard, soft buttons | No | L | — |  | No |
| I15 | Gamepads (browser, touch gamepad, uinput) | No | — | — |  | No |


## U. Cursor

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| U1 | Remote cursor as a CSS cursor: PNG, hotspot, sharp on HiDPI screens | Yes | S | R | Browsers limit CSS cursor size. Chrome allows at most 128 px and blocks large cursors near the window edge. | Yes |
| U2 | Cursor drawn on an overlay, for agent moves and cursors that are too large | Yes | S | R |  | Yes |

## B. Clipboard

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| B1 | Text, both directions | Yes | M | R |  | Yes |
| B2 | Images (PNG), both directions | Yes | S | R | Needs B1. | Yes |
| B3 | HTML, both directions | Yes | S | R | Needs B1. | Yes |
| B4 | Connect the RMNG clipboard broker (offer, then request, then data) to the browser: read the local clipboard when the window gets focus, and fetch the remote bytes as soon as an offer arrives | Yes | M | R | The browser can only use the clipboard while its window has focus. | Yes |
| B5 | When the browser refuses a write, try again on the next click or key press | Yes | S | R |  | Yes |
| B6 | Send clipboard bytes as binary WebSocket messages, not base64 JSON | Yes | S | R |  | Yes |
| B7 | Clipboard policy (in only, out only, off) and "secret" clipboard | No | — | — |  | No |

## M. Monitors and windows

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| M1 | One browser window per monitor, opened from the ViewSpec | Yes | M | R | Each window can have its own WebSocket for its monitor, or one window can feed the others. Decide this in the prototype. | Yes |
| M2 | Put each window on its screen and make it fullscreen (Window Management API) | No | M | R | Removed after round 9: windows open as plain windows, and the operator places, sizes and fullscreens them. | No |
| M3 | Drag across the edge between two monitors (`drag_route.rs`) | Yes | L | K | The windows are separate, so they must coordinate (for example with `BroadcastChannel`). | Yes |
| M4 | Resize the remote monitor to the browser window | No | M | R | Depends on Q5. RMNG uses fixed layout presets today. | No (Q5) |
| M5 | Scale to fit (letterbox) when the window is smaller than the monitor | Yes | S | R |  | Yes |
| M6 | HiDPI: draw the stream at device pixels, so a monitor that fits the screen is sharp | Yes | S | R | Q5: the monitor size comes from the layout preset. The viewer does not change it. | Yes (Q5) |
| M7 | Clone switching: the dashboard selects a clone, the viewer windows follow the ViewSpec | Yes | S | K | The server already re-sends the cached frame. | Yes |
| M8 | The Selkies limit of 2 displays and its `#display2` URL scheme | No | — | — | RMNG supports any number of monitors. | No |

## O. Other Selkies features

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| O1 | Drop a file on the viewer to upload it to the clone's home folder `~/` | Yes | M | R | Q6, A3. No download (A4): use the SMB share. | Yes (Q6, A3, A4) |
| O2 | Printing to the browser | No | — | — |  | No |
| O3 | Apps panel (install and start apps) | No | — | — |  | No |
| O4 | Sharing links: view-only, collaborator, players 2–4 | No | — | — |  | No |
| O5 | Selkies dashboards (sidebar, settings, translations) | No | — | — | We use the RMNG React frontend. | No |
| O6 | Viewer toolbar in the RMNG frontend: fullscreen, pointer lock, stats, open monitor windows | No | M | R | Removed after round 9: F11 and Ctrl+Alt+G/P do the same; the dashboard opens the windows. A Fullscreen button came back at the top left, because macOS takes F11; in fullscreen it shows only on hover. | No |

## N. Native viewer features that Selkies does not have

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| N1 | Terminal for clones with no desktop (tmux; tag 7 out, tags 3–5 in) | Yes | M | D | xterm.js (MIT). | Yes |
| N2 | Port forwarding: replace it with a dialog where the user types a local port and a clone port, and copies the generated `ssh -J rmng@<host>:2222 -N -L <local>:localhost:<clone-port> rmng@<clone>` command | Yes | S | K | Q3, A2. Nothing is stored. Replaces `PortForwardModal.tsx`. Reuse `lib/ssh.ts`. | Yes (Q3, A2) |
| N3 | Frame-rate test tool (`viewer --headless`) | Yes | S | R | Replace it with a small Rust WebSocket client. | Yes |
| N4 | Chroma setting (4:2:0 or 4:4:4) in Settings → Video | No | — | K | Q4. Deleted in X4. | No (Q4) |

## X. The switch

| ID | Work | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| X1 | Delete `crates/viewer`, `crates/viewer-macos` and `crates/viewer-core` (move the parts we keep first), the port 9001 listener, and the unused types in `wire/src/viewer.rs` | Yes | S | X |  | Yes |
| X2 | Update README, `docs/PROTOCOL.md`, `docs/DEPLOY.md`, Dockerfile ports, compose | Yes | S | X |  | Yes |
| X3 | Remove viewer build dependencies (GTK) from the Dockerfile and toolchain | Yes | S | X |  | Yes |
| X4 | Delete the 4:4:4 (AVC444) path: `wire/src/avc444.rs`, `media/src/glpack.rs`, the `chroma` config field and its Settings control | Yes | S | X | Q4. | Yes (Q4) |
| X5 | Delete port forwarding on the server: port 9005 listener, `control-server/src/forward.rs`, `wire/src/forward.rs`, the stored `PortForward` rules on each clone, `PUT /api/hosts/:id/forwards`, and the `forwards` SSE event | Yes | S | X | Q3, A2. Old `state.json` files keep a `forwards` field on clones: serde must ignore it. | Yes (Q3, A2) |

---

## Size of the proposal

These counts include only the **Yes** rows.

| | S | M | L |
|---|---|---|---|
| Rows | 34 | 12 | 1 |

That is about 8–10 weeks of work for one engineer, or less with work done in parallel. The largest risk is M3
(dragging between monitor windows).

## Implementation rounds

**Status: rounds 1–9 are done.** The browser viewer is the only viewer. The native viewers, ports 9001 and 9005,
the server-side port forwarding and 4:4:4 encode are removed.

Work happens on `main` in `/home/rmng/RMNG`. Each round is one worker agent, then one reviewer agent. The
orchestrator commits the worker's result, and the reviewer's findings go into the next worker's task. Nothing is
pushed or published.

The order is horizontal: round 1 builds every layer thinly, so the whole path lines up. Later rounds add features
on top of it. Port 9001 and the native viewers stay until round 7, so we can compare both viewers.

| Round | Content | Rows |
|---|---|---|
| 1 | **Skeleton.** Server: WebSocket route on port 9000 with the browser protocol, same per-viewer rules as port 9001. Wire: one message type per direction, exported to TypeScript. Browser: viewer page per monitor, opened from the dashboard; worker with WebSocket, reconnect, WebCodecs decode and display; HTTPS warning; stats overlay. All other message types arrive at the page but do nothing yet. | T1–T4, T7, T17, C1–C3, C5, C6, C9 |
| 2 | **Fixes from review 1, then an end-to-end check on CT 101**: build the image, load it on CT 101 without publishing it, watch a clone in Chrome, compare frame rate with the native viewer. | — |
| 3 | **Input and cursor**, and the viewer toolbar. | I1–I4, I7–I13, U1, U2, O6 |
| 4 | **Clipboard and file upload.** | B1–B6, O1 |
| 5 | **Multi-monitor.** | M1–M3, M5–M7 |
| 6 | **Native-viewer features**: terminal, port-forward command dialog, frame-rate test tool. | N1–N3 |
| 7 | **The switch**: delete the native viewers, ports 9001 and 9005, stored forward rules, 4:4:4, and update the docs. | X1–X5 |
| 8 | **Final end-to-end check on CT 101** and fixes. | — |
| 9 | **Clean-up, fanned out.** Several read-only agents each search one area (control-server media plane, wire types, clone-daemon, frontend, CLI and scripts, Dockerfile and docs) for logic that only existed for the native viewers or port 9001 and can now be simplified or removed. Workers then apply the agreed cuts by area, and one reviewer checks the result. | — |
