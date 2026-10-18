# Protocol, config & internals reference

Everything below the HTTP/MCP layer: the browser viewer protocol, the clone↔server
unix socket, the config schema, every environment variable, the clone-daemon CLI, and each
crate's public Rust API. Sources: [crates/wire/src/socket.rs](../crates/wire/src/socket.rs),
[browser.rs](../crates/wire/src/browser.rs), [config.rs](../crates/wire/src/config.rs),
[control-server/src/mediaplane.rs](../crates/control-server/src/mediaplane.rs),
[control-server/src/webviewer.rs](../crates/control-server/src/webviewer.rs),
[clone-daemon/src/transport.rs](../crates/clone-daemon/src/transport.rs),
[media/src/sock.rs](../crates/media/src/sock.rs).

## Ports & sockets

| Name | Default | Override | Listener | Connected by | Transport |
| --- | --- | --- | --- | --- | --- |
| web | `9000` | — (`wire::PORT_WEB`) | control-server web | browser (dashboard + viewer) / `rmng` CLI / control-client | HTTP + SSE + WebSocket (`/api/viewer`) |
| bastion | `2222` | — (`wire::PORT_BASTION`) | control-server ssh.rs | operator ssh client (jump) | OpenSSH; pubkey-only; forwards to `clone:22` |
| daemon MCP | `9004` | `RMNG_DAEMON_MCP_PORT` | clone-daemon | agents in the clone + `rmng desktop` proxy (via web API) | HTTP JSON-RPC |
| clone socket | `/srv/rmng-sock/clones.sock` | — (`wire::CLONE_SOCKET`, server) / `RMNG_SOCKET` (daemon) | control-server mediaplane | clone-daemon | unix `SOCK_SEQPACKET` + `SCM_RIGHTS` |
| holder socket | `$XDG_RUNTIME_DIR/rmng-session-holder.sock` | `RMNG_HOLDER_SOCKET` | session holder (in clone) | clone-daemon | unix `SOCK_SEQPACKET` |

---

## Browser viewer protocol (browser ⇄ control-server)

Each viewer window opens one WebSocket on the web port, same origin as the page:
`ws://<host>:9000/api/viewer?monitor=<id>`, or `wss://` when the page is HTTPS. The
socket carries the video of that one monitor and every control message. Source:
[browser.rs](../crates/wire/src/browser.rs) (types),
[webviewer.rs](../crates/control-server/src/webviewer.rs) (socket),
[mediaplane.rs](../crates/control-server/src/mediaplane.rs) (fan-out).

**Binary messages.** Byte 0 is the kind.

Video (kind `0`, server → browser): a 10-byte `VideoHeader`, then one Annex-B access unit.
All numbers are big-endian.

| Bytes | Field |
| --- | --- |
| `0` | kind, always `0` |
| `1` | flags, bit 0 = keyframe |
| `2..6` | monitor id (u32) |
| `6..10` | frame sequence number of the monitor's encoder (u32, wraps) |
| `10..` | the H.264 access unit (Annex-B) |

Clipboard data (kinds `2` and `1`, both directions): one `ClipboardData` reply, as raw bytes,
not as base64 in JSON, cut into pieces of at most 64 KiB (`encode_clipboard_piece` /
`decode_clipboard_piece`). The pieces of one reply go in order; each has kind `2`, except the
last, which has kind `1`. A reply that fits in one piece is one message of kind `1`. A piece
of another serial or MIME type starts a new reply: the sender stopped the old one (below).

| Bytes | Field |
| --- | --- |
| `0` | kind: `2`, or `1` for the last piece |
| `1..9` | offer serial (u64) |
| `9..11` | MIME type length `n` (u16) |
| `11..11+n` | MIME type, UTF-8 |
| `11+n..` | the piece's bytes (a reply with no bytes = the owner could not read them) |

The pieces yield to video and to the other messages. The server sends a piece only when no
video waits in the socket's queue and the window has room as for a video frame (below: room,
or a piece that waited 100 ms while the window is at most 1 s behind), and only while the bytes
on the way (video and pieces the window has not acknowledged with `clip` in `ack`) stay within
what the window acknowledged lately in one floor (below) and 100 ms; one piece may always be on
the way. The path holds one floor of it, so a large copy delays a frame by about 100 ms, also
through a proxy, and on the LAN it goes at the link's speed. The other way, the browser keeps
the pieces on the way within the rate the server's `clip_ack`s show times the lowest round trip
of one piece and 100 ms (and always one), so the window's acknowledgements and input wait about
100 ms behind an upload. (`bufferedAmount` alone is not enough: it does not see
what waits in the kernel and on the link, and on a 1 Mbit/s uplink the acknowledgements
waited 2 to 7 s behind an upload.) The delivered rate is measured over periods of 500 ms.
A newer copy stops a transfer that still runs: the server drops the pieces not sent yet
when any endpoint offers a new copy, and the window drops its own when it offers one or sees
an offer. A window drops a reply it got only part of when it sees or makes an offer, and
keeps only the pieces of the replies it asked for since then.

**Text messages** are JSON. The `t` field names the message (snake_case).

`ToBrowser` (server → browser):

| `t` | Body | Meaning |
| --- | --- | --- |
| `scroll` | `{mode, speed}` | The active layout preset's scroll mode (`auto`, `trackpad` or `mouse`) and speed in percent (a decimal; a negative speed scrolls the other way). Sent at connect and whenever either changes; the window applies it at once. |
| `version` | `{build_id}` | The server's build, first on every connection. A window that reconnects and sees a different one reloads. |
| `view_spec` | `ViewSpec {monitors: ViewMonitor[], content}` | The window set and what it shows. `content.mode` is `desktop`, or `terminal` with `clone` and `sessions` for a clone with no desktop. Sent at connect and on every selection or layout change. Empty `monitors` = nothing is selected. |
| `cursor` | `CursorMeta` | The remote cursor (position, shape on change, `warp`). Sent only to the windows of its `monitor_id`. |
| `clipboard` | `ClipboardMsg` (`k`: `offer` / `request`) | The clipboard broker. `data` never goes as JSON; it goes as binary pieces. |
| `term_data` | `TermData {session, data /* b64 */}` | Terminal output of one tmux session. Sent only to the windows of monitor 0, which show the terminal. |
| `echo` | `{id}` | The answer to the browser's `echo`, with the same `id`. |
| `clip_ack` | `{n}` (u32) | How many clipboard pieces from the browser arrived on this socket (wraps). Sent for each piece. |

`FromBrowser` (browser → server):

| `t` | Body | Meaning |
| --- | --- | --- |
| `input` | `InputMsg` (`kind`, below) | An input event for the **selected** clone. |
| `clipboard` | `ClipboardMsg` (`k`: `offer` / `request`) | The window's clipboard offer or request. Data goes as binary pieces. |
| `term_input` | `TermInput {session, data /* b64 */}` | Keys or pasted bytes for one tmux session. |
| `term_resize` | `TermResize {cols, rows}` | The terminal size. Every session is resized to it. |
| `term_new_session` | — | Make a new tmux session (the "+" tab). |
| `request_keyframe` | `RequestKeyframe {monitor_id}` | Re-encode the monitor's last frame as a keyframe: after a decode error. Ignored for a monitor other than the socket's own (`?monitor=`). |
| `ack` | `{seq, held_ms, clip, decoding}` (u32 each) | The newest video frame the decoder output (`seq` from its header; a frame dropped while the window waits for a keyframe counts when it arrives), how long, in ms, the window held that number before this message, how many clipboard pieces arrived on this socket (`clip`, wraps; 0 when left out), and how many frames wait in the window's decoder (`decoding`, its `decodeQueueSize`; 0 when left out). Sent every 50 ms when `seq` changed, at once when a clipboard piece arrives, and once a second otherwise. Before the first frame (a terminal, or no video yet) `seq` is 0, which the server ignores. Below. |
| `echo` | `{id}` (u32) | The server answers with `echo` and the same `id`, ahead of the video that waits in this socket's queue. The time to the answer is the round trip plus the wait in the kernel send buffer and in any proxy on the way. `rmng-fps` uses it; the viewer does not send it. |

**Connect.** The server queues the view spec, the last cursor of this monitor and the
current clipboard offer, then re-encodes the last frame of this monitor as a keyframe. A
selection change sends a new view spec, the cursors and a keyframe to every window.

**Keyframes** come only on demand: a join, `request_keyframe`, a selection change. The
encoder adds a periodic one only after 1024 frames (`key-int-max`, its highest value). A keyframe goes to every window of that monitor,
so the server makes at most one per monitor per 250 ms. A request inside that time is not
dropped: it gets its keyframe at the end of it.

**Socket options.** Port 9000 turns off Nagle's algorithm (`TCP_NODELAY`) and uses BBR
congestion control when the kernel has it (`tcp_bbr`), else the kernel's default. The log
says which one at start (`web port TCP congestion control: …`). Each socket keeps at most
128 KiB of unsent data in the kernel (`TCP_NOTSENT_LOWAT`), so video waits in the server's
queue, where control messages can go before it.

**Acknowledgements.** The server stamps each video frame when the socket's writer hands it
to the socket. On each `ack` it ends every frame up to `seq` and takes one round trip: the
time since the stamp of `seq`, less `held_ms`. The window acknowledges a frame when its
decoder has output it, so the round trip includes the decode, and a slow decoder holds frames
back as a slow link does. The floor is the lowest round trip of the last 1 to 2 minutes, of
deltas sent while no keyframe was out and with no queue in front of them: the delta is at most
100 ms of the path's rate (the highest delivered rate of the last 1–2 minutes, below), and with
the video and clipboard bytes on the way when it was handed to the socket at most one floor and
100 ms of it (a path with no queue holds one floor of it). Not the last delivered rate: that is
only what the screen sent, and with a round trip over 100 ms the bytes on the way are more than
100 ms of it whenever frames flow. A round trip behind more has a queue in it, and a large frame is its own queue (on the
hotspot link a lone 297 KB delta took 2.7 s, and that floor let 5 s of frames onto the path).
Before the first rate is measured (500 ms of `ack`s), no delta counts. An `ack` whose `held_ms` is longer than its frame has
been out names an earlier frame with the same number (a new encoder counts from 1 again) and
is ignored. Off the LAN the browser's TCP connection often ends in a proxy (`tailscale
serve`), so the acknowledgements are the only signal that sees the whole path. At debug level
(`rmng_control_server::webviewer`) the server logs one line per socket every 5 s while video
or clipboard bytes move: round trip, video delay (p50 and p95 of each acknowledged frame's
time from the server's queue to its `ack`, less `held_ms`), floor, how far behind, frames and
KB on the way, frames queued, the delivered Mbit/s, the clipboard KB sent, and captured frames
that were never encoded (below). `rmng_control_server::mediaplane` logs each
`request_keyframe`.

**Queue.** Each socket has one queue with three lanes: control messages (every text message)
go first, then video, then clipboard pieces (above). A window is **behind** by the time its
oldest video frame that is not acknowledged (queued, or on the way) has waited since it was
queued, less the floor. Nothing in the queue is dropped: the encoder makes no frame for a
window without room (below), so at most two frames wait in its video lane, plus the keyframes
that joins and requests make. A queue over 64 MiB (two of the largest clipboard replies)
closes the socket.

**Encode only when there is room.** A captured frame goes into the monitor's encoder only
when every window of that monitor has room: at most one frame waits in its video lane, and the
window is at most the floor plus 100 ms behind (its oldest frame on the way has been out at
most two round trips and 100 ms: one round trip for TCP to repair a lost packet, and
acknowledgements come up to 50 ms late). A frame that has waited 100 ms needs only up to
1 s behind instead, so a link that is not far behind gets at least 10 frames per second. A
keyframe on the way counts as any frame, so the frames behind a slow keyframe do not fill the
path (with free room while a keyframe was out, the frames sent behind a slow join keyframe
on the hotspot link were 2 s late, and in one bench run the floor rose to 2.3 s). Else the
server keeps only the newest captured frame. It looks at that frame again only at the next
captured frame, `ack`, or window that leaves; there is no timer. So when the screen stops
after motion, the last frame can wait up to about one round trip and 50 ms (the next `ack`).
The floor leaves
out keyframes, the deltas sent while a keyframe was out, and deltas sent behind a queue (above). On a link slower than the video
the frame rate falls to what the link carries, and no keyframe is needed: a frame that was
never encoded is not in the reference chain. The slowest window sets the frame rate for all
windows of the monitor. A window that stops acknowledging holds the others back until it is
closed (10 s, below). Keyframes are made whatever the room. Why the rule has its parts:
[WAN-BENCH.md](WAN-BENCH.md) (rounds 3 and 4).

**Automatic bitrate.** The encoder runs VBR with a QP from 25 to 35. With no limit (50 Mbit/s)
every frame gets QP 25, as the fixed quality (CQP 23/25) of earlier rounds gave: that is the
LAN behaviour. Once a second, on an `ack`, the server looks at each monitor's encoder. Only a
second in which the screen made at least 15 frames (encoded, or replaced while they waited for
room) counts: a still screen, a caret or typing says nothing about the link. When the slowest
window of that monitor is shown (acknowledges) fewer than 15 frames per second for 5 s, the
server limits the bitrate to 70 % of what that window delivered (the median of those seconds;
each second takes the video and clipboard bytes its `ack`s ended in the last 500 ms period,
and none when no `ack` ended bytes for 2 s). 5 s, so that a loss repair on a lossy link (no
`ack` for 1–2 s) is not taken for a slow link; one second counts for at most 1.5 s, also after
a gap in the `ack`s. A second in which a keyframe was queued or on the way to a window does not
count and starts the 5 s again: on a slow link a join's keyframe holds the frames behind it
back for seconds. A low second in which the slowest window's decoder held more than 2 frames
(`decoding`) is the browser's (a slow decoder, a hidden tab), not the link's, and does not
count. The QP then rises, at most to 35 so that text stays readable: a frame that needs more
bytes than its share is larger, and the frame rate falls again (quality first, then frame
rate). A window that still gets under 15 frames per second lowers the limit again, by at least
20 % and not under 500 kbit/s, unless the frames are over their share already (a lower limit
would not make them smaller). After 5 s that show at least 24 frames per second, or all but
one of the frames the screen made, the limit goes up by 1.25, but only while the frames use at
least 80 % of it (below that VBR gives QP 25 anyway); a limit that reaches 50 Mbit/s is no limit.
A calm screen keeps the limit. Each change makes the encoder start again with a keyframe, so
the limit changes at most once per 5 s. The VA rate control counts 30 frames per second, so
the limit is really a size per frame (limit / 30), and radeonsi keeps each frame near it unless
the QP reaches 35: at a 700 kbit/s limit, text frames were 2.7 KB (at most 4 KB) and a picture
pan at QP 35 7–8 KB, also right after a still period. Its buffer (`cpb-size`) is fixed at
150,000 kbit; a smaller one made more bytes, not fewer (`media::encode`). The server logs each
change at info level (`monitor 1 bitrate: no limit → 1400 kbit/s (…)`). A new encoder (a new
monitor size, a server restart) starts with no limit; a window that reconnects keeps the
encoder and its limit.

**Timeouts.** A socket with video frames or a clipboard piece on the way and no `ack` that
moves for 10 s is closed: the browser stopped reading. One send has no time limit. The server
sends a ping every 5 s. A socket that receives nothing (pongs and acks included) for 10 s is
closed. One WebSocket message may be up to 32 MiB; a clipboard reply is at most 32 MiB.

**Reconnect.** When a socket on which the window got `version` closes, the window connects
again at once; after each try that fails (also one the server accepts and closes at once) it
waits 400 ms, doubled each time up to 1 s. The new socket is primed
again. Input made while the socket is down waits in the window and goes when the new socket
opens: pointer moves in a row become the newest one, relative moves in a row add up, and
input more than 2 s old is dropped, except key and button releases.

**Held input.** When the selection moves to another clone, or the socket closes, the server
releases the keys and buttons this socket still holds down in the old clone.

**File upload.** `POST /api/viewer/upload?name=<file name>` with the raw file as the body
writes the file into the selected clone's home (`~/`). A name that exists gets ` (1)`,
` (2)`… before its extension. The reply is `{name}`, the name used.

`InputMsg` ([socket.rs](../crates/wire/src/socket.rs), serde tag `kind`, snake_case):

| Variant | Fields | Use |
| --- | --- | --- |
| `pointer_move` | `monitor_id`, `x`, `y` (f64) | absolute pointer in monitor-pixel space (**native**, see below) |
| `pointer_relative` | `dx`, `dy` (f64) | unaccelerated delta — pointer-lock / games |
| `button` | `button` (evdev: `0x110`–`0x112` left/right/middle, `0x113`/`0x114` back/forward), `pressed` | mouse button |
| `axis` | `axis` (0=vert,1=horiz), `step` (±1) | discrete scroll (mouse wheel notch) |
| `axis_continuous` | `dx`, `dy`, `flags` | smooth (trackpad) scroll |
| `key` | `keysym` (X11), `pressed` | text/modifier key (MCP `key` path) |
| `key_code` | `keycode` (Linux evdev, from `KeyboardEvent.code`), `pressed` | physical-key identity |

The viewer sends `key_code` for keys, `pointer_move`/`button`/`axis`/`axis_continuous` for
the mouse, and `pointer_relative` while the pointer is locked (Ctrl+Alt+G, or automatically
while the clone hides its cursor). When the server sends a `CursorMeta{warp:true}` (an
MCP-driven move) the viewer snaps the drawn remote cursor and **ignores local motion for
~0.5 s** so the operator's mouse does not fight the agent.

> **This path is native.** Coordinates here are the monitor's real pixels, and the H.264
> stream is encoded at full resolution. The 1080p virtual space is a property of the
> **daemon MCP tool channel only** ([MCP.md](MCP.md)) — the daemon converts virtual → native
> before it ever reaches this socket. Do not change the viewer or the encoder to 1080p.

**Frame-rate test.** `rmng-fps` ([rmng-fps.rs](../crates/control-client/src/bin/rmng-fps.rs))
connects to `/api/viewer` like a viewer window and prints once a second how many frames,
keyframes and megabits arrived, and the delay (the median `echo` time). It does not decode,
so it sends `ack` for a frame when the frame arrives; the server's log then has the video's
own delay (the `echo` answer goes ahead of the video, so its time is not the video's delay).
Given a number of seconds, it stops after them and prints a summary. When the server closes
the socket, it connects again after 1 s. The test bench uses it ([WAN-BENCH.md](WAN-BENCH.md)).

---

## Clone socket protocol (clone-daemon ⇄ control-server)

A unix `SOCK_SEQPACKET` socket (one JSON message per datagram). dmabuf file descriptors ride
out-of-band via `SCM_RIGHTS` in the same datagram, in plane order — never in the JSON. The
daemon connects to `RMNG_SOCKET`; the server listens on `wire::CLONE_SOCKET`
(`/srv/rmng-sock/clones.sock`, a constant, written into every clone's `RMNG_SOCKET` at
bootstrap). The socket lives in
the shared **`rmng-sock` named volume**, mounted at the same path `/srv/rmng-sock` into the
control-server **and** every clone (a named volume, not a bind, so siblings can share it);
`chmod 0777` so cross-uid clones connect.

**Handshake:** the daemon's first message is `DaemonMsg::Hello { clone_id, fresh_session }`.
`fresh_session` is true when this daemon had to start the session holder, so the desktop
behind it is seconds old and holds no window anyone placed. The server answers those with the
active layout preset even when the clone is not the selected one, because a holder that
remembered nothing comes up on the built-in single 1920x1080. The field defaults to false, so
a daemon older than it reads as "the holder was already running" and is left alone.

`DaemonMsg` (daemon → server), serde tag `t`:

| Variant | Payload | Meaning |
| --- | --- | --- |
| `hello` | `{clone_id, fresh_session}` | register the clone; `fresh_session` asks for the active layout |
| `frame` | `FrameMsg` | one captured monitor frame; dmabuf fds attached via SCM_RIGHTS |
| `cursor` | `CursorMeta` | cursor position (+shape on change, +`warp` if MCP-driven) |
| `layout` | `{monitors: MonitorPlacement[]}` | the monitors the clone has after it applies a layout (`{id}` each); the server drops the encoders of monitors that are gone |
| `clipboard_offer` / `clipboard_request` / `clipboard_data` | resp. types | clipboard bridge |

`ServerMsg` (server → daemon), serde tag `t`: `ack
{monitor_id, seq}` (flow control — the daemon waits for the ack before the next frame),
`input(InputMsg)`, the three `clipboard_*` messages, `capture {active}` — start or stop
the clone's capture (the server keeps it on only for the selected clone while a viewer is
connected), and `set_monitors {monitors:
MonitorSpec[]}` — apply a layout **live**: the daemon does a make-before-break session swap
(rebuilds a fresh Mutter session with the desired monitors, switches capture + input to it,
then stops the old one, so running apps never close). Sent to the selected clone: on its
`Hello`, on `POST /api/layout/activate`, on a `PUT /api/config` that moves the active preset's
geometry, and on the `POST /api/activate` that brings it on screen. Sent to any clone, watched
or not, on a `Hello` that carries `fresh_session`, and once to a brand-new clone when its
daemon first registers. Every other clone keeps the layout it was last viewed with.

`FrameMsg`: `monitor_id`, `fourcc` (DRM, e.g. `0x34325241` "AR24"), `modifier` (DRM format
modifier), `width`, `height`, `planes: [{offset, stride}]`, `seq` (echoed in `ack`).

### CursorMeta

```rust
struct CursorMeta {
    monitor_id: u32, x: i32, y: i32,
    shape: Option<CursorShape>,   // only on shape change; positions carry None
    warp: bool,                   // #[serde(default)] — true = server/MCP-initiated warp
}
struct CursorShape { width, height, hotspot_x, hotspot_y, rgba: Vec<u8> /* base64 in JSON */ }
```

Captured out-of-band as `SPA_META_Cursor` (cursor-mode METADATA, via the raw-PipeWire path
since GStreamer `pipewiresrc` can't surface it) and drawn client-side. `warp:true` triggers
the viewer's 0.5 s local-motion suppression.

### Clipboard (rich + lazy)

`ClipboardOffer {serial, mime_types[]}` advertises types (no bytes). `ClipboardRequest
{serial, mime_type}` asks for one. `ClipboardData {serial, mime_type, bytes /* b64 */}`
transfers. `ClipboardMsg` (serde tag `k`: `offer`/`request`/`data`) is the viewer-side
envelope. The control-server is the **broker**: it tracks the owner, fans each offer to
every viewer window + every other clone, routes a paste's request to the owner, and routes
bytes back to the requester. The clone-daemon bridges via Mutter `RemoteDesktop` selection
(`SelectionRead`/`SelectionWrite`); the browser viewer via the async clipboard API, which
Chrome allows only while the window has focus.

**An image does not fit in one message, on either hop.** Both had a ceiling sized for text,
and a pasted screenshot crossed neither.

The clone socket is `SOCK_SEQPACKET`, and the kernel refuses a datagram over `SO_SNDBUF - 32`,
about 208 KB at the default `net.core.wmem_default`. A 256 KB payload is 349,596 bytes of JSON,
so `sendmsg` returned `EMSGSIZE` and the broker discarded the error. A message past the ceiling
is now split into 64 KiB chunks framed `\0RMC | id | index | count | slice` and rejoined by the
receiver. The magic starts with a NUL, which no JSON document can, so a whole message is still
sent exactly as it was and needs no envelope. The receive queue holds about three chunks, so a
chunked send waits for the peer to drain between datagrams, bounded at 10 s.

On the viewer socket, clipboard bytes go in binary pieces of 64 KiB that yield to video
([above](#browser-viewer-protocol-browser--control-server)).

**A paste that gets no bytes fails after 15 s** rather than hanging. Mutter cancels the
transfer requests it has no answer for 15 s after the last one, so a longer wait would not help:
a 4 MB image from a browser window on a 1 Mbit/s uplink (about 35 s) cannot be pasted. Mutter opens the pipe and
the pasting application blocks on it until somebody writes and calls `SelectionWriteDone`, so a
reply that never arrives is not a dropped paste, it is a frozen one, and what the application
eventually shows is whatever it had. That is what made a too-large image read as slow and then
corrupt instead of as an error.

**One read at a time.** Mutter runs a single `SelectionRead` per session and fails a second
one with `LimitsExceeded: Tried to read in parallel`, which reaches the requester as empty
bytes. One copy fans out to every endpoint, and each asks per MIME, so two rules keep the
reads sequential. The broker forwards only the first request for a `(serial, mime_type)` and
adds later requesters to the same pending list, and the clone-daemon holds a gate across the
whole transfer, from `SelectionRead` to the end of the fd read.

---

## Holder socket protocol (clone-daemon ⇄ session holder)

Two processes inside each headed clone. `rmng-session-holder.service` owns the Mutter
RemoteDesktop and ScreenCast sessions, the virtual monitors, `ApplyMonitorsConfig`, input
injection and the clipboard bridge. `rmng-clone-daemon.service` owns capture, shipping,
screenshots, and the MCP on `:9004`. Video encode runs on the control-server. Both are the same binary, the holder started with
`--session-holder`. Source: [holder.rs](../crates/wire/src/holder.rs).

**Why they are split.** Mutter destroys a RemoteDesktop session when the D-Bus connection
that created it drops, and gnome-shell remaps every window when the monitor set empties. The
daemon restarts on every payload push, so a daemon that owned the session reset every window
position on each update: measured on a 1920x1080 clone, a window at (100,100) came back at
(63,57), and on a two-monitor layout every floating window collapsed onto monitor 0. The
holder does not restart on a payload push, so the monitors never go away and nothing moves.

**Why input and the clipboard cross the socket.** Mutter answers session methods only for the
connection that created the session: the same call from another connection returns
`org.freedesktop.DBus.Error.AccessDenied`. `org.gnome.Shell.Eval` has no such check, so the
window tools (`list_windows`, `move_window`) stay in the daemon on its own bus connection.
Capture stays there too: a PipeWire node is reachable by id from any client on the clone's
socket, verified against a node another process created.

**Messages**, one JSON object per datagram, chunked over 64 KiB exactly as the clone socket
is. No file descriptors cross.

| Direction | Message | Meaning |
| --- | --- | --- |
| daemon → holder | `hello{proto}` | First message. Answered by `hello_ok`. |
| daemon → holder | `input` | One `InputMsg` to inject. Fire-and-forget, ordered. |
| daemon → holder | `set_layout{monitors}` | Apply a layout, make-before-break. No-op if unchanged. |
| daemon → holder | `capture_ready{generation}` | Capture is live on that generation's nodes. |
| daemon → holder | `clipboard_offer` / `clipboard_request` / `clipboard_data` | Relayed from the broker. |
| holder → daemon | `hello_ok{proto, generation, monitors}` | The monitor set already held. |
| holder → daemon | `monitors{generation, monitors}` | A new set exists and is capturable. |
| holder → daemon | `swap_done{generation}` | The old monitors are gone; drop their capture. |
| holder → daemon | `clipboard_offer` / `clipboard_request` / `clipboard_data` | Relayed to the broker. |

**A layout swap is a three-step handshake**, so no frame gap opens in the middle of it. The
holder builds the new session alongside the old and sends `monitors`. The daemon starts
capture on the new nodes and answers `capture_ready`. The holder stops the old session, waits
for its connectors to actually disappear, applies the layout, and sends `swap_done`, at which
point the daemon drops the old captures. Waiting for the connectors is not optional: applying
a config against dying ones is silently ignored by Mutter at best and crashed gnome-shell
fleet-wide once.

**Version skew.** The daemon compares `PROTO_VERSION` with the holder's and restarts the
holder exactly once on a mismatch, which costs one window reset on that release and nothing
on the others. Nothing else restarts the holder: the reconcile pass starts and enables it,
never restarts it.

**The layout the holder boots on** comes from `~/.rmng/monitors`, written after every applied
layout, and falls back to `RMNG_MONITORS` on a clone that has never run one. A value baked
into a clone image goes stale the first time the operator switches presets, and booting on it
costs a session build and a swap that the control-server's push then has to undo.

---

## Config schema

`AppConfig` loads from `./config.json` in the working directory (no env override — the Docker
image sets `WORKDIR /data`, so `config.json` + `data/` land in the `rmng-data` volume);
written at `0600`. The web API returns `AppConfigRedacted`, which carries the preset Linear
keys verbatim as `linearKey: string` (the clients are what call Linear) and withholds nothing
else: every other credential the server holds lives in its own account store rather than in
the config. `PUT /api/config` returns
`{ config: AppConfigRedacted, networkWarning?: string }`. Source:
[config.rs](../crates/wire/src/config.rs).

Ports, the data directory, the clone socket and the Docker socket are constants in
`wire::config` (`PORT_WEB`, `PORT_BASTION`, `PORT_DAEMON_MCP`, `DATA_DIR`, `CLONE_SOCKET`,
`DOCKER_SOCKET`), not config fields. Keys from older files (`listen`, `dataDir`,
`staticDir`, `cloneSocket`, `chroma`) are ignored when `config.json` loads.

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `setup_complete` | bool | `false` | latched `true` by the first-run setup wizard; gates the frontend to the wizard until then |
| `layout_presets` | `LayoutPreset[]` | `[]` | named monitor-layout presets (`{name, monitors: MonitorSpec[], scrollMode, scrollSpeed}`); the operator switches the active one from the sidebar (`POST /api/layout/activate`) |
| `active_layout` | string | `""` | name of the active layout preset; drives `effective_monitors()` (see below) |
| `docker` | `DockerConfig` | see below | hostname prefix + per-clone limits + ZFS parent dataset for clone homes |
| `presets` | `Preset[]` | `[]` | clone presets: env vars + Linear key + auto-select ticket-id prefixes (the key is a credential, and `GET /api/config` vends it to the clients) |
| `claude` | `ClaudeConfig` | — | usage polling config |
| `groups` | `CloneGroup[]` | `[]` | the single named account-pool list (Claude + Codex members mixed); a clone binds at most one pool, each rotator side only sees its own provider members (not secret) |
| `agent_playbook` | string | shipped default | the person's ticket procedure and app quirks for the assistant (how to drive a desktop is the assistant's own knowledge), sent with the preset's append in the `[From RMNG]` header of each new assistant chat's first message (see [API.md](API.md#per-clone-assistant-chat)); nothing is written into the clone. Seeded from [agent-playbook.md](../crates/wire/src/agent-playbook.md); editable in Settings; **non-secret** (passes through the redacted view); applies to the next new chat |
| `judge` | `JudgeConfig` | `gpt-5.6-luna` | working-vs-stuck detection: `codexModel` (default `gpt-5.6-luna`) and `codexEmail` (`null` = the first imported Codex account). No credential of its own: the calls run on that account's ChatGPT plan, using the token the server already holds to run its clones. Nothing secret, so the whole struct passes through the redacted view. No Codex account imported ⇒ no clone is ever reported `working` (see [monitorState](API.md#monitorstate)). Settings → Agents |
| `assistant` | `AssistantConfig` | `{url: "", serverUrl: ""}` | the chat panel's assistant: `url` is the pi-web origin, `serverUrl` this server's web API origin as the assistant reaches it (written into each new chat's first message for `rmng --server`). Empty `url` ⇒ the chat panel refuses to send; empty `serverUrl` ⇒ it refuses to start a new chat. Plain addresses, so the struct passes through the redacted view. Settings → Presets → Assistant |

- **`DockerConfig`** (no secret, so the whole struct passes through the redacted view):
  `hostname_prefix` (`"pega-"`, editable in Settings → prepended to derived clone hostnames;
  carried from the retired `proxmox.hostname_prefix` on migration), `clone_cpus` (`16` —
  whole cores → `nano_cpus`) and `clone_memory_mb` (`32768` — MiB, +8 GiB swap), both
  editable per-clone limits, and `homes_parent` (the parent ZFS dataset of the clone homes).
  The Docker socket is the constant `wire::DOCKER_SOCKET` (`/var/run/docker.sock`) and the
  `rmng` bridge subnet is the constant `wire::DOCKER_SUBNET` (`10.99.0.0/24`; addressing is
  Docker DNS — clones resolve by container name, the control-server by its `rmng-control`
  alias). Retired keys in an old file (`socket`, `subnet`, `templateReference`, …) are
  ignored on load.
- **First-run setup wizard**: a fresh deploy ships `config.json` with `"setupComplete":
  false`, so the web UI shows the wizard (environment checklist → server settings + monitors
  → finish) instead of the dashboard; finishing latches
  `setupComplete: true` (a one-way latch) and materializes the lazy `rmng` network. There is
  **no grandfather rule**: an old `config.json` re-runs the wizard (new machine, no network /
  template pulled yet). A legacy `proxmox` block is scrubbed on load, carrying `hostnamePrefix` into
  `docker.hostname_prefix`; old `state.json` clones load as plain unmanaged rows
  (`managed: false`; serde drops the stale `ctid`/`container` keys).
- <a id="preset"></a>**`Preset`**: `name`, `labels` (ticket-id prefixes / Linear team keys,
  e.g. `DEV`, that auto-select this preset when cloning from a ticket — matched
  case-insensitively against the ticket's prefix like `DEV-196` → `dev`, first match in
  config order wins), `linear_key` (personal API key — injected into the clone as
  `LINEAR_API_KEY` authing its `linear` MCP, and handed to the clients verbatim by
  `GET /api/config`, which is what the ticket column lists issues with and what the clone
  dialog and the `rmng` ticket verbs resolve a ticket with), `vars`
  (env vars written to the clone's `/etc/environment`), and `agent_playbook` (optional,
  non-secret — text appended after a blank line to the global `agent_playbook` for
  clones of this preset; empty ⇒ global only). `PUT /api/config` merges rows by name
  (blank `linearKey` keeps the stored one; omitted row deletes). One-shot migration at
  load: legacy `envPresets` seed `presets` (no labels/keys); legacy per-workspace `linear`
  keys are dropped (re-enter per preset in Settings).
- **`ClaudeConfig`**: `poll_secs` (`600`, floored 15), `pinned_email?`.
- <a id="claude-accounts"></a>**Claude accounts** live outside config, in the server's 0600
  secret store `claude-accounts.json`: per account an OAuth pair (`access_token` +
  single-use `refresh_token`, both **secret**), obtained by signing in to the provider at
  the server (`POST /api/login/begin` then `/api/login/complete`).
  The server owns the whole refresh lifecycle; a clone gets **only the current short-lived
  access token** written into its `~/.claude/.credentials.json` (refresh emptied, far-future
  expiry), re-pushed to every assigned clone whenever a refresh rotates it — so a *running*
  clone hot-swaps without restart (written via `docker exec` into the clone).
- **`CloneGroup`**: `name`, `accounts` (member emails). A clone bound to a group
  (`Clone.claude_group`) sticks to its account (preserving its prompt cache) until that
  account is exhausted (80% 5h or 95% 7d) or leaves the group; the 10-min rotator then
  moves it to the least-loaded / least-used member. Selected at clone/swap time as
  `group:<name>`. A clone selected as `auto` rotates across all imported accounts using
  this same sticky rotation.

<a id="codex-accounts"></a>**Codex accounts** — server-owned single-token model, identical in spirit to Claude accounts.

- **Store:** `codex-accounts.json` (0600, in `data_dir`; override `RMNG_CODEX_ACCOUNTS_FILE`).
  Each record: `id` (`codex:<account_id>`), `email`, `account_id`, `plan`, `access_token`,
  `id_token`, `refresh_token`, `expires_at`.
- **Injected in-clone file:** `~/.codex/auth.json` = `{ "OPENAI_API_KEY": null, "tokens":
  { "id_token", "access_token", "refresh_token": "", "account_id" }, "last_refresh": <now> }`.
  The refresh token is emptied and `last_refresh` set to now so the clone's CLI never
  rotates the server-owned token. The server re-pushes on every refresh, with a 60-min lead.
- **Refresh:** `POST https://auth.openai.com/oauth/token` (client_id
  `app_EMoamEEZ73f0CkXaXp7hrann`). No `expires_in` — expiry is decoded from the access-token
  JWT `exp`. Refresh tokens are single-use / rotating.
- **Usage:** `GET https://chatgpt.com/backend-api/wham/usage` (Bearer + `ChatGPT-Account-Id`);
  windows map to 5h/weekly by `limit_window_seconds`.
- **`CodexConfig`**: `auto_reset` (bool, default `false`).
- **`groups`** (`CloneGroup[]`): the single pool list above. A clone binds one pool via its
  clone-level `group` (each side resolving to its own provider's least-used member); a legacy
  per-side `group:<name>` selection still binds it. Selected at clone/swap time as the shared
  `group` field.
- **`Clone`** carries `codexAccountEmail` / `codexGroup` / `codexSelection` alongside the
  Claude equivalents. One clone can hold both a Claude and a Codex account simultaneously.

- **`MonitorSpec`**: `width`, `height`, `x`, `y`, `primary`.
- **`LayoutPreset`**: `name`, `monitors: MonitorSpec[]`, `scrollMode` (`auto` guesses mouse wheel or trackpad per event, `trackpad` and `mouse` take every event as one; default `auto`), `scrollSpeed` (percent, 10–1000, default 100) — a full named arrangement the
  operator can switch to live. `AppConfig::effective_monitors()` returns the active preset's
  (`active_layout`) monitors, falling back to the first preset, then a built-in dual
  2560×1440 default when `layout_presets` is empty.

Template params are mostly not config: the base OS is fixed in the template build
(`ubuntu:26.04` in `template/Dockerfile` — the patched gnome-shell is compiled against 26.04's
GNOME only). Clone images build on demand from each preset's Dockerfile into a hash tag
(`POST /api/images/prebuild` warms one without creating). Per-clone CPU / memory limits come
from `docker.clone_cpus` / `docker.clone_memory_mb`, applied at clone create — not per image.

---

## Environment variables

**control-server:** all config is `./config.json` in the working directory (the Docker
image sets `WORKDIR /data`, the `rmng-data` volume). Ports, the clone socket and the Docker
daemon socket are constants in `wire::config`. It reads these env vars, none of which is a
setting:

- `RUST_LOG` — log filter; default `info,tower_http=warn,clip=debug`, also set in the image.
- `RMNG_ENC_LATENCY` — when set, the encoder logs the time from frame push to encoded frame.
  For measurement only.
- `RMNG_CODEX_ACCOUNTS_FILE` — the path of the Codex account store, in place of
  `codex-accounts.json` in the data directory. A test uses it.

The ignored test `gpt_answers_the_real_question` also reads `RMNG_CODEX_AUTH` (the path of a
Codex `auth.json`); the server does not.

**clone-daemon:** `RMNG_SOCKET` (media socket; required unless `--session-holder`),
`RMNG_CLONE_ID` (id; default hostname), `RMNG_MONITORS` (boot-default layout CSV, below —
corrected to the config's active layout preset by the server's `SetMonitors` on a new clone's
first registration, and thereafter whenever the clone is the selected one),
`RMNG_DAEMON_MCP_PORT` (`9004`), `RMNG_DESKTOP_HEIGHT` (`1080`; height of the MCP's virtual
coordinate/screenshot space — `0` serves everything at native res; see [MCP.md](MCP.md)),
`RMNG_HOLDER_SOCKET` (holder socket path; default
`$XDG_RUNTIME_DIR/rmng-session-holder.sock`), `RUST_LOG`.

---

## clone-daemon CLI

Source: [clone-daemon/src/main.rs](../crates/clone-daemon/src/main.rs).

**Shipping mode (default, no argument):** connect to the media socket named by
`RMNG_SOCKET`, connect to the session holder, capture the monitors it holds, ship dmabuf
frames + cursor, relay input to the holder, and serve the daemon MCP on `:9004`. Without
`RMNG_SOCKET` it exits with an error.

**`--session-holder`:** run as the session holder instead. It RecordVirtuals the boot layout,
holds it open across the daemon's restarts, and injects input and clipboard on the daemon's
behalf. See the holder socket protocol above.

**`RMNG_MONITORS` format:** comma-separated `WxH+X+Y[*]` (offset optional; trailing `*` =
primary; first is primary if none marked). E.g. `1920x1080+0+0*,1280x1024+1920+0`. Empty →
one 1920×1080 primary. The unique `WxH` sizes also seed `MUTTER_DEBUG_DUMMY_MODE_SPECS`. This
env var is now only a **boot default** baked into the clone template, read by the session
holder and only when `~/.rmng/monitors` has no remembered layout. The shipped holder unit
carries no layout of its own, one unit going to every clone, so a payload push writes the
active preset into `~/.rmng/monitors` when the file is absent. That is what keeps a clone
upgrading onto the holder from building its first session on the built-in single monitor.

The server corrects a stale layout with `ServerMsg::SetMonitors` carrying
`config.effective_monitors()` (the active layout preset), live and without a restart. That
happens once for a brand-new clone when its daemon first registers, on any `Hello` that
reports a freshly started holder, and after that only while the clone is the one on screen:
its `Hello`, a `POST /api/layout/activate`, and the `POST /api/activate` that selects it.

---

## Per-crate public API

**`wire`** ([lib.rs](../crates/wire/src/lib.rs)) — pure types, no I/O. Modules: `config`
(`AppConfig` & friends, `AppConfigRedacted`), `control` (`ControlState`, `Clone`, `Operation`,
`Chat`/`ChatMessage`, `ClaudeUsage`, `MonitorSpec`, the enums), `socket` (clone-socket
protocol), `browser` (browser viewer protocol), `mcp` (MCP arg DTOs). control, config and
browser types derive `ts_rs::TS` and export to `frontend/app/lib/wire/`.

**`media`** ([lib.rs](../crates/media/src/lib.rs)) — the GPU + socket plane:

- `Encoder::new(on_au: FnMut(Vec<u8>, bool))` / `.push(fd, fourcc, modifier, w, h)` /
  `.force_idr()` — one VA-API H.264 encoder per monitor (`vapostproc ! vah264enc ! h264parse`,
  Annex-B AUs to the callback).
- `screenshot_jpeg(fd, fourcc, modifier, w, h) -> Vec<u8>` — one-shot dmabuf→JPEG.
- `Listener::bind(path)` / `.accept() -> Conn`; `Conn::recv() -> (DaemonMsg, Vec<OwnedFd>)` /
  `.send(&ServerMsg)` — the clone-socket transport (SCM_RIGHTS).

**`control-client`** ([lib.rs](../crates/control-client/src/lib.rs)) — `Client::new(base)`,
`Client::state_once() -> ControlState`: a thin reqwest+SSE client for integration tests.
Its `rmng-fps` binary is the viewer frame-rate test (see the browser viewer protocol above).

**`clone-daemon`, `control-server`** are binaries (no library API); their internal
modules are described in their crate READMEs.
