# Browser viewer off the LAN: scope

The browser viewer from [WEB-VIEWER-SCOPE.md](WEB-VIEWER-SCOPE.md) is done and works well on the LAN. This document
lists what we can change so that it also works well over the internet (Tailscale from home, a café, a phone hotspot).
Each row gives a proposal, an effort label and the decision. All rows are decided: the operator accepted every
proposal after the answers to W1–W6.

The sources are Selkies (`selkies-project/selkies` at `6b9f17a`, 2026-10-16), pixelflux (the Selkies capture and
encode library, `linuxserver/pixelflux` at `59c523a`) and the RMNG code on `main` at `edac4e5`.

## What goes wrong today off the LAN

These are the facts that the rows below fix. The file references are to RMNG code.

1. **Nothing limits the bitrate.** The encoder uses fixed quality (CQP, QP 23 for keyframes, QP 25 for other
   frames) and has no bitrate setting (`crates/media/src/encode.rs:65`). A test with the same encoder settings on a
   2560×1440 screen gave about **10 Mbit/s** while one window scrolls and about **25 Mbit/s** while the whole screen
   moves. A 1920×1080 monitor needs about 0.56× these numbers.
2. **A keyframe comes every 30 frames** (`key-int-max=30`). That is 2 keyframes per second during motion, about
   190 KB each at 1440p, so about 3 Mbit/s of the 10 Mbit/s above.
3. **A slow link gives seconds of delay, then a disconnect.** Each window has a queue of 128 messages
   (`mediaplane.rs:70`), which is about 2 s of video at 60 fps. Under that queue, the Linux send buffer can hold
   several MB more. When the link is slower than the video, the delay grows to seconds. Then the queue is full, the
   server closes the socket, the browser reconnects after 1 s and gets a keyframe, and the cycle starts again.
4. **Nagle's algorithm is on** for port 9000 (`web.rs:153`, no `.tcp_nodelay(true)`). The end of each frame and
   small messages (cursor, clipboard offer) can wait for an acknowledgement from the browser: up to one round trip.
5. **Large messages block the video.** Clipboard data goes as one message of up to 23–32 MiB on the same socket as
   the video, and the browser fetches it as soon as the clone copies something (B4). A single message that needs
   more than 10 s closes the socket (`SEND_TIMEOUT`, `webviewer.rs:35`). At 10 Mbit/s that is about 12 MB.
6. **Each cursor move goes to every window.** One `cursor` message per pointer move goes to every monitor window,
   not only to the window that shows that monitor (`mediaplane.rs:1157`).
7. **The server measures nothing** about the link: no round-trip time, no delivered bitrate, no queue delay.

What already works well off the LAN:

- **Damage-driven capture.** A still screen sends no frames at all.
- **Cursor drawn locally.** The cursor is a CSS cursor in the browser, so it moves at local speed whatever the delay.
- **Separate upload.** File upload uses its own HTTP connection, not the video socket.

## Finding after round 1: the off-LAN path goes through `tailscale serve`

Off the LAN, the operators open the viewer through `tailscale serve` on the CT (`https://<ct>.<tailnet>.ts.net`,
proxied to `http://127.0.0.1:9000`). CT 204, CT 205 and CT 206 all do this. The browser's TCP connection ends in
`tailscaled`, and `tailscaled` opens a second connection to port 9000. So:

- Socket options on port 9000 (L1 Nagle off, L2 small send buffer, L3 BBR) act only on the local hop. They help on
  the LAN and on a direct connection, but not on the internet hop.
- The kernel queue of our socket (Q1) drains at once into `tailscaled`'s own buffers. A slow link shows there only
  after those buffers are full, which can be seconds.
- The only signal that sees the whole path is an acknowledgement from the browser (R2). Selkies uses the same
  signal, and it works behind any proxy.

Changes to the rows: R2 moves into round 2. Q1 and Q4 use the acknowledgements, and the kernel queue is only an extra
signal. R1 waits for the acknowledgements. The bench (S1) puts a proxy on CT 101 in front of port 9000 and slows the
proxy's port, as `tailscale serve` does.

## How Selkies handles a slow link (short summary)

Selkies uses one WebSocket for everything, as we do. In WebSocket mode it adds four automatic controls, all on by
default:

1. **Frame acknowledgements.** Every 50 ms the browser sends the newest frame id it received. The server uses this
   to measure the round-trip time, its lowest value over 10 minutes (the "floor"), and the delivered bitrate.
2. **A gate.** When the queue on the path is more than 250 ms longer than the floor, the server stops sending
   frames to that browser.
3. **Automatic bitrate.** The encoder uses a constant bitrate (CBR, 8 Mbit/s by default). Every 0.5 s the bitrate
   goes down to 0.5–0.85 × the delivered rate when a queue builds, and up by 1.15× per second when the path is clear.
4. **Repair without a keyframe.** Selkies writes its own H.264 headers (pixelflux drives VA-API directly, not
   through GStreamer). When a frame is dropped, the encoder predicts the next frame from an older frame that the
   browser still has.

Selkies also uses keyframes only on demand. Each client has its own queue with a byte budget. Clipboard data goes in
16 KiB pieces, paced so that it does not delay the video. When the screen stops, Selkies encodes it again a few
times at better quality ("paint-over").

**Two limits that matter for us:**

- **Bitrate changes cost a keyframe.** With VA-API, each bitrate change forces a keyframe, in pixelflux and in our
  GStreamer `vah264enc`. I checked `gstvah264enc.c` (1.26): setting `bitrate` while the encoder runs drains it and
  restarts the GOP. Selkies changes the bitrate up to twice per second, so on AMD it sends many keyframes.
- **No repair through GStreamer.** `vah264enc` cannot predict from an older frame, so we cannot copy item 4
  without writing our own encoder (K4).

## Recommended direction

Do not drop frames after the encoder. **Encode less often when the link is full** (R1).

Our capture is damage-driven and the server already keeps the newest captured frame. When a viewer has data waiting
to be sent, the server does not give new frames to the encoder. When the viewer has room again, the server encodes
only the newest frame. Results:

- The frame rate falls to what the link can carry.
- Each frame keeps full quality, so text stays sharp.
- The delay stays near one round trip.
- No keyframe is needed, because a frame that was not encoded is not in the reference chain.

pixelflux does the same thing between its encoder and Selkies.

To make this work, the server must see when a viewer has data waiting, and the data must wait in our queue, not in
the Linux send buffer (L2, Q1). For links where even one frame per round trip is too much (a phone hotspot, W1), the
automatic bitrate (R2, R4) lowers the quality, but only after R1 has lowered the frame rate (W3).

## Questions

| ID | Question | Why it matters | Proposal | Answer |
|---|---|---|---|---|
| W1 | Which link must work well? For example: Tailscale from home, 20–80 ms round trip, 10–50 Mbit/s from the server to you. Or also a phone hotspot, 100–300 ms, 2–5 Mbit/s, with loss. | Sets the default numbers and the test bench (S1). Below about 5 Mbit/s, R1 alone gives a low frame rate during large motion, so R3 or R4 becomes necessary. | Home and office first. Phone hotspot "usable, not smooth". | Phone hotspot too, and it must be smooth there. |
| W2 | Who decides the bitrate? (a) You, with a setting (R3). (b) The server, automatically (R2, R4). (c) Both: automatic, with your setting as the maximum. | (b) and (c) need frame acknowledgements (R2) and a congestion controller (R4), about 2 weeks. (a) is 1–3 days. | (a) now, with R1 doing the automatic part through the frame rate. (c) later if R1 is not enough. | (b) Automatic only. No user setting. |
| W3 | On a full link, what must go down first: frame rate (R1) or picture quality (R4)? | Lower frame rate keeps text sharp and costs no keyframe. Lower quality keeps motion smooth, but with `vah264enc` each change costs a keyframe. | Frame rate first. | Frame rate first. R4 lowers the quality only when the frame rate stays low. |
| W4 | Can two browsers on different links watch the same monitor at the same time (for example a LAN desktop and a laptop at home)? | There is one encoder per monitor for all viewers. With R1, the slowest viewer sets the frame rate for all of them. Avoiding that needs per-viewer frame dropping with repair, which means our own VA-API encoder (K4, more than 2 weeks). | Rare. The slowest viewer sets the frame rate. A viewer that is stuck for 10 s is disconnected (Q4). | The slowest viewer sets the frame rate and the bitrate for all viewers of that monitor. |
| W5 | Bring back a small stats line (round-trip time, Mbit/s, frames shown per second, delay) and a "poor connection" mark? You removed the stats view after round 9 (C9 in the old scope). | Without numbers we cannot tune the off-LAN behaviour or see why a session is slow. | Yes, hidden by default, shown with a shortcut. The "poor connection" mark always on. | Neither. Tuning uses the test bench and server logs. |
| W6 | Where does the quality or bitrate setting (R3) live: one global value in Settings → Video, or one value per layout preset, like the scroll mode and scroll speed? | You already have a "Laptop" preset, so a per-preset value maps to "I am away from home". | Per layout preset. | Does not apply: no user setting (W2). |

## Legend

Same as [WEB-VIEWER-SCOPE.md](WEB-VIEWER-SCOPE.md):

- **Proposed:**
  - **Yes**: build it.
  - **Later**: build it when a need appears.
  - **No**: do not build it.
  - **Ask**: depends on a question ID.
- **Effort:**
  - **S**: less than 1 day.
  - **M**: 1–3 days.
  - **L**: 1–2 weeks.
  - **XL**: more than 2 weeks.
- **How:**
  - **R**: our own code from the idea.
  - **P**: copy a Selkies or pixelflux file (MPL-2.0, keeps its header).
  - **K**: change existing RMNG code.

---

## L. Link and socket

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| L1 | Turn off Nagle (`TCP_NODELAY`) on port 9000 | Yes | S | K | One line: `axum::serve(…).tcp_nodelay(true)` (axum 0.7.9 has it). Selkies gets it from asyncio, and Chrome sets it on its side already. | Yes |
| L2 | Keep the Linux send buffer small for unsent data (`TCP_NOTSENT_LOWAT`, about 128 KiB) | Yes | S | R | Today several MB can wait in the kernel, where we cannot drop or reorder anything. With this option, the data waits in our queue instead, where Q2, Q3 and R1 can act on it. Selkies does not set it: it reads the kernel queue size instead (`SIOCOUTQNSD`). Set it on each accepted socket, by wrapping the listener. | Yes |
| L3 | BBR congestion control on server sockets (`setsockopt TCP_CONGESTION "bbr"`) | Yes | S | R | Today the server uses CUBIC, which cuts throughput hard on random loss (Wi-Fi, mobile) and fills queues. If BBR is not available, the server keeps the default. The CT shares the Proxmox kernel, so `tcp_bbr` must be loaded on the host (`modprobe tcp_bbr`). Today only `reno cubic` are available. | Yes |
| L4 | WebTransport (HTTP/3 over UDP) instead of WebSocket, so that one lost packet delays one frame, not all data after it | No | XL | R | Needs a certificate on the server itself. `tailscale serve` (Q1 in the old scope) does not carry UDP. With L2 and R1, the delay after a loss stays short. | No |
| L5 | A second socket for input | No | — | — | Input goes from the browser to the server and video goes the other way, so they do not wait for each other. The server-to-browser control messages are handled by Q2. | No |

## Q. Server queue for each viewer

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| Q1 | Measure how far each viewer is behind: frames sent and not yet acknowledged (R2), beyond the round-trip floor; plus bytes in our queue and unsent bytes in the kernel (`SIOCOUTQNSD`) as an extra signal | Yes | S | R | Needed by Q3, R1 and D4. Selkies uses the same kernel number. | Yes |
| Q2 | Control messages go before video in the queue (cursor, `view_spec`, clipboard offer, `version`, `scroll`) | Yes | S | K | Today one queue holds everything in order of arrival. A queued frame cannot be taken back once written, so this works only together with L2. | Yes |
| Q3 | Replace the "128 messages, then close" rule (A1 in the old scope) with a budget in time: when a viewer is more than about 1 s behind, drop its queued frames, keep the socket, and send it a keyframe | Yes | M | R | This changes A1. With R1 in place, this happens only after a sudden drop of the link. The keyframe goes to all viewers of that monitor, because they share one encoder. Limit it to one keyframe per second. Built in round 2, removed in round 4: with R1, at most one or two small deltas wait in our queue, and a resync swapped them for a keyframe many times larger through the same slow socket. The 64 MiB queue limit and Q4 stay. | Yes (removed in round 4) |
| Q4 | Close a socket only when it makes no progress for 10 s (no acknowledgement from the browser, R2), instead of when one write takes 10 s | Yes | S | R | Selkies reads `tcpi_bytes_acked` from `TCP_INFO`. Today a big clipboard message on a slow link closes a socket that still works. | Yes |

## R. Rate: how much the encoder sends

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| R1 | **Encode only when the viewers have room.** When the slowest viewer of a monitor has more than about one frame waiting (Q1), do not give new frames to the encoder. Keep the newest captured frame and encode it when the viewer has room. | Yes | M | R | The core of this scope. The frame rate adapts to the link, quality stays the same, and no keyframe is needed. The newest frame must still be encoded when the screen stops, or the browser keeps an old picture. The server already keeps the last frame for joining windows (`mediaplane.rs:900-941`). W3, W4. Built in round 3 with a time rule instead of a frame count; the rule is in [PROTOCOL.md](PROTOCOL.md#browser-viewer-protocol-browser--control-server) and why it has its parts in [WAN-BENCH.md](WAN-BENCH.md) (rounds 3 and 4). | Yes |
| R2 | Frame acknowledgements from the browser: newest frame id + how long it waited, every 50 ms, sent from the worker | Yes (W2) | M | R | Was T5 (No) in the old scope. Gives the round-trip time, its floor and the delivered bitrate. Needed by R4. The server writes the round-trip time and the rate to its log, for the test bench (W5). R1 does not need it, but it can use it to wait for "room on the path" and not only "room in the kernel". | Yes (W2) |
| R3 | Manual setting: quality (QP) or maximum bitrate | No (W2) | M | K | W2: no user setting. R4 uses a fixed highest bitrate in the code. | No (W2) |
| R4 | Automatic bitrate from R2 (Selkies `CongestionSteer`: on a queue, go down to 0.5–0.85 × the delivered rate; on a clear path, go up by 1.15× per second) | Yes (W2, W3) | L | R | Switches the encoder from CQP to CBR or VBR. W3: it lowers the bitrate only when R1 holds the frame rate low (for example under 15 fps) for some seconds. W4: the slowest viewer of the monitor drives it. Each change costs a keyframe with `vah264enc`, so change only in steps of at least 20 % and at most once per 5 s. Remember the last good rate in the browser, so a reconnect starts near it (Selkies `CC_RATE`). Helps when even one frame is too large for the link. | Yes (W2, W3) |
| R5 | Highest QP under CBR or VBR, so that text stays readable | Yes | S | K | Selkies suggests a highest QP of 35. Needed now that R4 is Yes. | Yes |
| R6 | Paint-over: when the screen stops for 0.25 s, encode the last frame again a few times so that it becomes sharp | Later | M | R | Was T14 (Later). Works only with CBR or VBR: our encoder cannot change the QP of one frame without a keyframe. pixelflux stops when the picture stops getting better, or after 30 s. Useful when R4 makes frames coarse on a hotspot (W1). | Later |
| R7 | Lower resolution for a slow link (encode at 0.5 or 0.75 scale, the browser scales up) | No | S | R | Use a smaller layout preset instead (for example "Laptop", 1710×1068). It gives sharp text at the size you see. Selkies does not scale automatically either. | No |

## K. Keyframes

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| K1 | Keyframes only on demand (no keyframe every 30 frames) | Yes | S | K | Was T13 (Later). Saves about 3 Mbit/s during motion at 1440p. The browser already asks for a keyframe at connect and after a decode error (T4, C5). Selkies does the same. | Yes |
| K2 | The server accepts at most one keyframe request per 250 ms per monitor | Yes | S | R | Today there is no limit, and each request makes a keyframe for every viewer of that monitor. | Yes |
| K3 | Browser: when a keyframe arrives behind more than 6 queued frames, reset the decoder and decode only the keyframe | Yes | S | R | Selkies `decode-gate.js`. Today the keyframe waits behind the old frames. | Yes |
| K4 | Repair a dropped frame without a keyframe (predict from an older frame; long-term reference on AMD) | No | XL | P | Was T12 (No). pixelflux proves that it works on radeonsi, but only with its own VA-API encoder in place of `vah264enc`: about 5,000 lines in `encoders/vaapi`, `h264.rs` and `reference.rs`, MPL-2.0. With R1 we drop no encoded frames, so we need it only for W4. | No |

## D. Browser

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| D1 | Draw only the newest decoded frame per screen refresh | Later | S | R | After a pause, frames arrive in a burst and today each one is drawn. The gain is small. | Later |
| D2 | Faster reconnect: first try at once after a socket that was open closes, then 400 ms doubling up to 1 s. Hold input during the reconnect (pointer moves merged, key releases kept) and send it after. | Yes (W1) | S | R | Selkies. Short drops are more frequent off the LAN. Today the browser waits 1 s and drops the input in between. | Yes (W1) |
| D3 | Decoder pace: the browser asks for a lower frame rate when its decoder is too slow | Later | M | P | Was T9 (Later). This is a slow computer, not a slow link. | Later |
| D4 | "Poor connection" mark on the viewer, decided by the server (Selkies: 30 % or more of the frames in 3 s were held back) | No (W5) | S | R | Was C10 (Later). Needs Q1 and R1. | No (W5) |

## V. Other traffic on the video socket

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| V1 | Send `cursor` messages only to the window of that monitor, and `term_data` only to the window that shows the terminal | Yes | S | K | Today each message goes to every window: with 2 monitors and 120 moves per second, that is 240 messages per second. | Yes |
| V2 | Clipboard data in pieces of 64 KiB, sent only when the video queue is short. Both directions. | Yes | M | R | Today one message of up to 32 MiB blocks the video. Selkies uses 16 KiB pieces and paces them. A newer copy cancels a transfer that is still running. Built in round 4 with one piece on the way at a time, each acknowledged by the other side: behind a proxy the server's socket never shows a queue, and the browser's `bufferedAmount` does not see the kernel's. | Yes |
| V3 | Cursor image as PNG, not raw pixels in base64 JSON | Later | S | K | Usually 3–12 KB, up to about 790 KB for a 384×384 cursor. Sent only when the shape changes. | Later |
| V4 | Merge pointer moves when the browser socket is backed up (more than 16 KiB unsent: one move per screen refresh, the newest wins) | Later | S | R | Selkies. Helps only when the upload from your side is slow, for example during a file upload. | Later |
| V5 | Pace file uploads so that they do not delay input | Later | M | R | Selkies `TransferPacer`. Same case as V4. | Later |

## S. Measurement and test

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| S1 | Test bench: slow-link profiles with `tc netem` and `tbf` on the test CT (for example 50 ms round trip and 10 Mbit/s; 150 ms, 3 Mbit/s and 1 % loss). Measure each round on these profiles and on the LAN. | Yes | S | R | Without it we cannot see whether a change helps, or whether it makes the LAN worse. W1: include the hotspot profile. W5: with no stats line, the server log (R2) and `rmng-fps` give the numbers. | Yes |
| S2 | Stats line: round-trip time, delivered Mbit/s, frames shown per second, frames not encoded (R1), queue delay. Hidden; a shortcut shows it. | No (W5) | M | R | Was C9 (No). S if R2 exists. | No (W5) |
| S3 | `rmng-fps`: add `wss://`, the round-trip time, and do not force a keyframe for the real viewers | Later | S | K |  | Later |

## Z. Other

| ID | Feature | Proposed | Effort | How | Notes | Decision |
|---|---|---|---|---|---|---|
| Z1 | Serve the frontend files compressed (gzip or brotli made at build time, `ServeDir::precompressed_gzip`) | Yes | S | K | Today the JavaScript is 1.8 MB and is sent uncompressed (`web.rs:124`). The browser keeps it in its cache, so only the first load is slow. Built in round 4 with both: brotli makes the text files about 4.5 times smaller. | Yes |
| Z2 | Dashboard events: send only what changed, not the full state (about 19 KB) on each change | Later | M | R | This is the dashboard tab, not the viewer. | Later |
| Z3 | Docs: check that Tailscale has a direct connection and not a relay (DERP), with `tailscale ping <host>` | Yes | S | — | A relay limits the bandwidth a lot. No RMNG code. | Yes |
| Z4 | Share one copy of each frame between all viewers (today 3–4 copies per frame) | Later | S | K | CPU only, not the network. | Later |

---

## Size of the proposal

These counts include only the **Yes** rows, after the answers to W1–W6.

| | S | M | L |
|---|---|---|---|
| Rows | 15 | 4 | 1 |

That is about 3–5 weeks for one engineer. The largest risk is R4 (automatic bitrate): each bitrate change costs a
keyframe with our encoder.

## Implementation rounds

**Status: round 1 is done (`4ea09ac`).**

Work happens on `main`. Each round is one worker agent, then one reviewer agent that checks correctness and
over-engineering. The orchestrator commits the worker's result, and the reviewer's findings go into the next
round's worker task. Nothing is pushed or published. CT 101 is the test deployment and the bench; the image is
loaded there with `docker save | docker load`, not published.

Each round is measured on the test bench (S1), off the LAN and on the LAN.

| Round | Content | Rows |
|---|---|---|
| 1 | **Test bench and quick fixes.** Measure today's behaviour first, then turn off Nagle, use BBR, send keyframes only on demand, and limit keyframe requests. | S1, L1, L3, K1, K2, Z3 |
| 2 | **Acknowledgements and queue.** Frame acknowledgements, small kernel buffer, measure how far each viewer is behind, control messages first, drop and resync instead of close, close only when stuck. A proxy hop in the bench. | R2, L2, Q1–Q4 |
| 3 | **Encode only when there is room.** | R1 |
| 4 | **Side traffic and browser.** Cursor only to its window, clipboard in pieces, keyframe flush in the browser, faster reconnect, compressed frontend files. | V1, V2, K3, D2, Z1 |
| 5 | **Automatic bitrate.** CBR or VBR with a highest QP, congestion control that acts only after R1 holds the frame rate low. | R4, R5 |
