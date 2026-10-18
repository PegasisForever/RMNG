# Viewer test bench: slow links

This bench measures the browser viewer on the LAN and on slow links, before and after a change.
It is row S1 of [WEB-VIEWER-WAN-SCOPE.md](WEB-VIEWER-WAN-SCOPE.md).

## Parts

| Part | Runs on | Does |
|---|---|---|
| [`scripts/wan-shape.sh`](../scripts/wan-shape.sh) | CT 101, as root | Starts a TCP proxy on port 9001 in front of port 9000, as `tailscale serve` is in production. Makes one port (9001 by default, or 9000) look like a slow link with `tc`. Other traffic is not shaped. |
| [`scripts/wan-motion.sh`](../scripts/wan-motion.sh) | the clone, as the clone user | Makes the screen still, or scrolls text in a window, or scrolls text on the full screen, or pans a detailed picture on the full screen. |
| `rmng-fps` ([PROTOCOL.md](PROTOCOL.md#browser-viewer-protocol-browser--control-server)) | the bench client | Joins as one viewer window and acknowledges frames as a window does. Prints each second, then for the run: frames, Mbit/s, keyframes, delay, reconnects. |

**The proxy path is the main path.** Off the LAN, operators open the viewer through
`tailscale serve`: the browser's TCP connection ends in `tailscaled`, which opens a second
connection to port 9000. The bench copies that with `socat` on port 9001 (its own buffers,
Nagle off on both sides, a second connection to `127.0.0.1:9000`) and shapes port 9001. The
server's socket then sees only the fast local hop, and the slow link is behind the proxy's
buffers, as in production. Shaping port 9000 instead measures the direct path.

`tc` makes the kernel load a missing qdisc, filter or action module, and a CT shares the
Proxmox host's kernel, so `wan-shape.sh apply` first checks `/proc/modules` and stops, with
the `modprobe` line for the host owner, when a module it needs is not loaded.

Profiles (each direction gets half of the round trip):

| Profile | Round trip | Down (server → viewer) | Up | Loss | Link buffer |
|---|---|---|---|---|---|
| `lan` | no shaping | | | | |
| `home` | 50 ms | 20 Mbit/s | 10 Mbit/s | 0 | 200 ms |
| `hotspot` | 150 ms | 3 Mbit/s | 1 Mbit/s | 1 % each way | 500 ms |

No jitter: `netem` jitter reorders packets, which a real link seldom does. (Round 1 used
±2 ms and ±10 ms.)

Motion (on monitor 1 of the default two-monitor 1920×1080 layout):

| Motion | Screen |
|---|---|
| `still` | Nothing moves. |
| `type` | A 400×100 window shows a new number 5 times per second, as typing does (from round 6). |
| `scroll` | A 960×540 window scrolls text at 240 px/s, 60 frames per second. |
| `large` | The same text scrolls on the full monitor: every frame changes. |
| `photo` | A picture full of fine detail (a Mandelbrot set) pans on the full monitor, 4 px right and 2 px down per frame: about 5.8 Mbit/s at 50 fps on the LAN, more than the hotspot link carries (from round 5). |

**Delay** is the time from an `echo` message to its answer, every 200 ms. Up to round 1 the
answer waited in the server's queue behind the video; from round 2 it goes ahead of the video
there. Either way it waits behind the video in the kernel send buffer, the proxy and the link
buffer. So the delay is the round trip plus the time the video waits on the way. It does not
include the encode or the decode.

**Video delay** (from round 2) comes from the server's log: one line per viewer every 5 s at
debug level (`rmng_control_server::webviewer=debug` in `RUST_LOG`), with the round trip of the
acknowledged frames, the floor, how far behind the viewer is, and resyncs. From round 3 the
line also has the **video delay**: for each acknowledged frame, the time from when the server
queued it for the socket (right after the encode) to its `ack`, less the time the viewer held
the number before it sent the `ack`. So it is a round trip that includes the wait in the
server's queue, the kernel, the proxy and the link, but not the encode or the decode. The
line gives its p50 and p95 for those 5 s; for a 30 s cell the table shows the median of the
p50 values and the highest p95 value. The `echo` delay goes ahead of the video in the
server's queue, so from round 2 it is not the video's delay. The same line counts the
captured frames that were never encoded (round 3, R1). From round 4 a browser window
acknowledges a frame when its decoder has output it, so for a window the video delay includes
the decode; `rmng-fps` does not decode and acknowledges a frame when it arrives. Round 4 drops
the resync count (resync is gone) and adds the clipboard KB sent.

## Run it

Copy the scripts in once, install `socat` in CT 101 and start the proxy. `$CLONE` is a clone
with a desktop.

```sh
SSH="ssh -i ~/.ssh/id_rsa -o IdentityAgent=none -o IdentitiesOnly=yes root@10.0.0.100"
CLONE=pega-speed-source
$SSH 'cat > /tmp/s && pct push 101 /tmp/s /root/wan-shape.sh --perms 755' < scripts/wan-shape.sh
$SSH 'pct exec 101 -- apt-get install -y socat && pct exec 101 -- /root/wan-shape.sh proxy start'
$SSH "cat > /tmp/m && pct push 101 /tmp/m /tmp/wan-motion.sh \
  && pct exec 101 -- docker cp /tmp/wan-motion.sh $CLONE:/tmp/wan-motion.sh" < scripts/wan-motion.sh
cargo build -p control-client --bin rmng-fps
```

Select the clone (the server sends only the selected clone), then run the matrix through the
proxy (for the direct path: `apply $profile 9000` and `rmng-fps 10.0.0.178`):

```sh
curl -s -XPOST http://10.0.0.178:9000/api/activate -H 'content-type: application/json' \
  -d "{\"id\":\"$CLONE\"}" >/dev/null
CT="$SSH pct exec 101 --"
for profile in lan home hotspot; do
  $CT /root/wan-shape.sh apply $profile
  for motion in still scroll large; do
    $CT docker exec -u rmng $CLONE bash /tmp/wan-motion.sh $motion
    sleep 3
    echo "$profile $motion: $(target/debug/rmng-fps 10.0.0.178:9001 1 30 | tail -1)"
  done
done
$CT docker exec -u rmng $CLONE bash /tmp/wan-motion.sh still
$CT /root/wan-shape.sh clear
$CT /root/wan-shape.sh proxy stop
```

Always end with `clear`; it removes only what `apply` added. `wan-shape.sh show` prints the
qdiscs and their counters. `wan-shape.sh down 1mbit` changes only the down rate of the applied
profile, in place, so nothing queued is lost: round 5 drops the hotspot to 1 Mbit/s for 20 s
and back to 3 (`down 3mbit`).

## Results

### Round 1: direct path

2026-10-17, CT 101, clone `pega-speed-source`, monitor 1, 30 s per cell. The bench client is a
clone on another CT; it reaches CT 101 over the LAN. The links had jitter (±2 ms home, ±10 ms
hotspot), and the shaped port was 9000. Each cell shows **baseline → round 1**.

- **Baseline**: `main` at `3a3bb4c`, plus the `echo` message (the delay needs it): a keyframe
  every 30 frames, Nagle on, CUBIC.
- **Round 1**: keyframes on demand (K1, K2), Nagle off (L1), BBR (L3).

| Link | Motion | fps | Mbit/s mean (peak) | Keyframes | Delay median / p95 (ms) | Reconnects |
|---|---|---|---|---|---|---|
| lan | still | 0.1 → 0.1 | 0.07 (2.08) → 0.00 (0.05) | 1 → 1 | 0 / 1 → 1 / 1 | 0 → 0 |
| lan | scroll | 50.0 → 50.0 | 1.82 (2.20) → 0.87 (1.41) | 50 → 2 | 0 / 29 → 0 / 1 | 0 → 0 |
| lan | large | 50.0 → 50.0 | 4.94 (6.18) → 1.61 (4.03) | 50 → 2 | 1 / 1 → 0 / 1 | 0 → 0 |
| home | still | 0.1 → 0.1 | 0.07 (2.09) → 0.07 (2.09) | 1 → 1 | 51 / 53 → 51 / 54 | 0 → 0 |
| home | scroll | 49.8 → 49.9 | 1.81 (2.25) → 0.88 (1.72) | 50 → 2 | 57 / 94 → 50 / 53 | 0 → 0 |
| home | large | 49.8 → 49.8 | 4.98 (6.96) → 1.61 (3.87) | 50 → 2 | 71 / 75 → 51 / 54 | 0 → 0 |
| hotspot | still | 0.1 → 0.1 | 0.07 (2.10) → 0.07 (2.10) | 1 → 1 | 154 / 503 → 155 / 449 | 0 → 0 |
| hotspot | scroll | 39.6 → 49.6 | 1.43 (2.35) → 0.88 (1.74) | 39 → 2 | 454 / 3836 → 157 / 358 | 1 → 0 |
| hotspot | large | 16.0 → 46.9 | 1.83 (3.60) → 1.53 (3.58) | 17 → 2 | 2816 / 7451 → 165 / 845 | 2 → 0 |

What the numbers show:

- On this text, the periodic keyframes were half to two thirds of the bitrate. Without them,
  full-screen scrolling needs 1.6 Mbit/s, so it fits in the hotspot's 3 Mbit/s. Before, the
  queue filled, the delay grew to seconds and the server closed the socket twice in 30 s.
- With Nagle on, a LAN scroll had a 29 ms p95 delay: small messages waited for an
  acknowledgement. With Nagle off, 1 ms.
- The two keyframes in round 1 are the one at join and the periodic one after 1024 frames.
- The peak of a still screen is the keyframe at join. Its size depends on what monitor 1
  showed last. On the hotspot, that keyframe also gives the 450–500 ms p95 delay.
- A keyframe request every 20 ms for 3 s gave 13 keyframes, 250 ms apart. The last one came
  10 ms after the last request, so a request inside the gap is served at its end.

The scope's 1440p test gave 10–25 Mbit/s for the same kind of motion. This text on a dark
background compresses better, so the bench uses less. A link that is slower than one frame per
round trip still needs rounds 2, 3 and 5.

BBR: the kernel of the Proxmox host had only `reno cubic` when the baseline ran. The first
round 1 build asked for `bbr` while the server ran privileged, so the kernel loaded `tcp_bbr`
on the host. The server now uses BBR only when the kernel already lists it, but the module
stays loaded, so the round 1 numbers are with BBR. The bench did not measure BBR on its own.

### Round 2: through the proxy

2026-10-17, same clone, monitor and client, 30 s per cell, through the proxy on port 9001 (the
port that is shaped), no jitter. Each cell shows **round 1 → round 2**. The same `rmng-fps`
(which sends `ack`) ran both; the round 1 server ignores `ack`.

- **Round 1**: the round 1 build already on CT 101 (image label `3a3bb4c-round1`).
- **Round 2**: frame acknowledgements (R2), 128 KiB `TCP_NOTSENT_LOWAT` (L2), control messages
  first (Q2), drop and resync when more than 1 s behind (Q1, Q3), close only when stuck (Q4).

**Video round trip** is from the round 2 server's log: the median and the highest of the 5 s
means of the frames' round trip (hand-off to the socket to `ack`, less the hold). The round 1
server has no acknowledgements.

| Link | Motion | fps | Mbit/s mean (peak) | Keyframes | Delay median / p95 (ms) | Video round trip median / max (ms) | Resyncs | Reconnects |
|---|---|---|---|---|---|---|---|---|
| lan | still | 0.1 → 0.1 | 0.00 (0.05) → 0.02 (0.59) | 1 → 1 | 1 / 1 → 1 / 1 | 1 / 1 | 0 | 0 → 0 |
| lan | scroll | 50.0 → 49.8 | 0.89 (1.62) → 0.89 (1.73) | 2 → 2 | 2 / 7 → 1 / 12 | 1 / 1 | 0 | 0 → 0 |
| lan | large | 50.0 → 49.9 | 1.61 (3.97) → 1.61 (3.98) | 2 → 2 | 1 / 11 → 1 / 4 | 1 / 1 | 0 | 0 → 0 |
| home | still | 0.1 → 0.1 | 0.07 (2.07) → 0.07 (2.07) | 1 → 1 | 51 / 92 → 51 / 92 | 218 (the join keyframe) | 0 | 0 → 0 |
| home | scroll | 49.9 → 49.8 | 0.88 (1.72) → 0.88 (1.75) | 2 → 2 | 86 / 105 → 77 / 98 | 85 / 86 | 0 | 0 → 0 |
| home | large | 49.9 → 49.7 | 1.61 (3.81) → 1.61 (3.75) | 2 → 2 | 92 / 112 → 92 / 96 | 83 / 85 | 0 | 0 → 0 |
| hotspot | still | 0.1 → 0.1 | 0.07 (2.08) → 0.07 (2.04) | 1 → 1 | 196 / 575 → 194 / 608 | 1333 (the join keyframe) | 0 | 0 → 0 |
| hotspot | scroll | 47.4 → 48.9 | 0.85 (1.22) → 0.87 (1.60) | 2 → 2 | 332 / 1607 → 275 / 1261 | 241 / 786 | 1 | 0 → 0 |
| hotspot | large | 44.9 → 35.3 | 1.47 (2.78) → 1.35 (4.21) | 2 → 5 | 1589 / 3202 → 1113 / 3141 | 1160 / 3067 | 4 | 0 → 0 |

For comparison, round 2 on the direct path (port 9000 shaped, hotspot, large): 49.5 fps,
1.60 Mbit/s, 2 keyframes, delay 261 / 706 ms, video round trip 229–396 ms, no resync.

What the numbers show:

- **The proxy hides the slow link from the server.** Through the proxy, the server's queue held
  0 frames in every log line: `socat` and its kernel buffers take everything at once, so up to
  650 KB (108 frames) were on the way on the hotspot. Only the acknowledgements saw it. On the
  direct path the same link kept about 17 frames (50–60 KB) on the way. The second TCP
  connection (proxy → viewer) uses the CT's default, CUBIC, not BBR. With 1 % loss each way it
  carries less than the 3 Mbit/s of the link, so the hotspot `large` motion (1.6 Mbit/s) did
  not fit. (In production that connection is `tailscaled`'s, so it can behave differently.)
- **Resync works, but cannot undo what the proxy holds.** On the hotspot, a viewer more than
  1 s behind lost its queued video and got a keyframe once the frames on the way were
  acknowledged. The median delay fell from 1.6 s to 1.1 s, with fewer frames (35 fps) and
  3 more keyframes. The p95 stayed at about 3 s: frames already in the proxy still had to
  cross. Not sending them in the first place is round 3 (R1), which waits for the
  acknowledgements before it encodes.
- **The LAN and the home link do not change.** No resync, the same frame rate and bitrate.
- `echo` now goes ahead of the queued video, but through the proxy the server's queue was empty
  anyway, so the delay column compares the two rounds directly.
- A real browser: headless Chrome on the bench client opened `/viewer?monitor=1` on port 9000
  (`--unsafely-treat-insecure-origin-as-secure`) while monitor 1 scrolled. The server log showed
  its acknowledgements: round trip 1 ms, 0.8–1.4 Mbit/s delivered, nothing behind.

### Round 3: encode only when there is room

2026-10-17, same clone, monitor and client, 30 s per cell, through the proxy on port 9001
unless the row says direct (port 9000 shaped, `rmng-fps 10.0.0.178`). Each cell shows
**round 2 → round 3**.

- **Round 2**: the round 2 code plus only the video delay in the debug line (image label
  `8ab9260-round2-meter`), so that both rounds have the same delay numbers.
- **Round 3**: R1 (image label `8ab9260-round3`). A captured frame is encoded only when each
  window of the monitor has room; else only the newest frame is held. The rule is in
  [PROTOCOL.md](PROTOCOL.md#browser-viewer-protocol-browser--control-server).

**Not encoded** is the captured frames that a newer frame replaced while they waited for room
(round 3 only). **Video delay** is the server's (above); **echo** is the `rmng-fps` delay.

| Link | Motion | Path | fps | Mbit/s mean (peak) | Keyframes | Not encoded | Resyncs | Reconnects | Video delay p50 / p95 (ms) | Echo p50 / p95 (ms) |
|---|---|---|---|---|---|---|---|---|---|---|
| lan | still | proxy | 0.0 → 0.0 | 0.00 (0.05) → 0.00 (0.05) | 1 → 1 | 0 | 0 → 0 | 0 → 0 | 1 / 1 → 1 / 1 | 1 / 1 → 1 / 1 |
| lan | scroll | proxy | 50.0 → 50.0 | 0.89 (1.73) → 0.89 (1.75) | 2 → 2 | 0 | 0 → 0 | 0 → 0 | 1 / 2 → 1 / 1 | 0 / 3 → 0 / 20 |
| lan | large | proxy | 50.0 → 50.0 | 1.61 (4.01) → 1.61 (4.00) | 2 → 2 | 0 | 0 → 0 | 0 → 0 | 1 / 2 → 1 / 2 | 1 / 14 → 0 / 6 |
| home | still | proxy | 0.1 → 0.1 | 0.07 (2.09) → 0.07 (2.09) | 1 → 1 | 0 | 0 → 0 | 0 → 0 | 221 / 221 → 228 / 228 | 51 / 92 → 51 / 92 |
| home | scroll | proxy | 49.9 → 49.9 | 0.88 (1.72) → 0.88 (1.74) | 2 → 2 | 0 | 0 → 0 | 0 → 0 | 85 / 108 → 84 / 107 | 85 / 108 → 84 / 108 |
| home | large | proxy | 49.9 → 49.8 | 1.61 (3.81) → 1.61 (3.82) | 2 → 2 | 1 | 0 → 0 | 0 → 0 | 68 / 103 → 79 / 106 | 82 / 98 → 79 / 96 |
| hotspot | still | proxy | 0.1 → 0.1 | 0.07 (2.08) → 0.07 (2.10) | 1 → 1 | 0 | 0 → 0 | 0 → 0 | 1154 / 1154 → 1112 / 1112 | 193 / 603 → 208 / 659 |
| hotspot | scroll | proxy | 49.5 → 45.1 | 0.88 (1.36) → 0.83 (1.14) | 2 → 2 | 132 | 0 → 0 | 0 → 0 | 244 / 695 → 231 / 742 | 276 / 626 → 255 / 571 |
| hotspot | large | proxy | 29.0 → 37.3 | 1.18 (3.23) → 1.40 (3.39) | 5 → 2 | 338 | 4 → 0 | 0 → 0 | 2132 / 4008 → 265 / 2112 | 1738 / 3767 → 307 / 1568 |
| hotspot | large | direct | 47.1 → 43.4 | 1.64 (4.40) → 1.50 (4.38) | 3 → 2 | 179 | 1 → 0 | 0 → 0 | 262 / 1645 → 243 / 1446 | 271 / 814 → 257 / 893 |

The lossy hotspot link changes much from run to run, so its cells ran again (round 2 once, round
3 twice, same code):

| Motion | Path | Round 2 | Round 3, run 2 | Round 3, run 3 |
|---|---|---|---|---|
| scroll | proxy | 46.7 fps, 1 resync, 243 / 1547 ms | 43.6 fps, 0 resyncs, 244 / 697 ms | 39.8 fps, 0 resyncs, 248 / 1177 ms |
| large | proxy | 31.4 fps, 4 resyncs, 1425 / 2464 ms | 36.7 fps, 0 resyncs, 260 / 1331 ms | 24.8 fps, 0 resyncs, 395 / 2011 ms |
| large | direct | 47.3 fps, 1 resync, 235 / 1213 ms | 42.5 fps, 0 resyncs, 238 / 1664 ms | 45.4 fps, 0 resyncs, 238 / 1162 ms |

(fps, resyncs, video delay p50 / p95.)

What the numbers show:

- **The LAN and the home link do not change.** Same frame rate, bitrate and delay; at most one
  frame in 30 s was not encoded.
- **Through the proxy on the hotspot, the full-screen scroll now stays near one round trip.**
  The video delay p50 fell from 1.4–2.1 s to 0.26–0.4 s, with no resyncs and one keyframe
  less per resync. Round 2 sent every frame into the proxy's buffers; round 3 holds the
  frames back that the link cannot carry yet, and encodes the newest one when it can.
- **The p95 stays at 1.2–2.1 s.** TCP loss repair on the 1 % loss link stops the
  acknowledgements for a round trip or more, and the frames on the way then wait. Frames that
  are already on the way cannot be taken back.
- **Direct path:** the same as round 2, without resyncs.

**What did not work, and why the rule has its parts.** The first rule held a frame while the
window was more than 100 ms past the floor, with no other limit. On the hotspot it made the
large scroll worse: 2–6 fps, 2.3–2.7 Mbit/s, 1–2 s delay. On this text, a frame that covers
more motion costs more than its share of bytes: the encoder follows only short motion (the
same scroll needs about 1.6 Mbit/s at 50 fps, 2.3 at 10 fps, 2.7 at 2 fps). So each hold made
the next frame larger, which caused the next hold. Three parts stop this:

- A frame waits at most 100 ms while the window is less than 1 s behind (at least 10 fps).
- Nothing is held while a keyframe is on the way. Before, the first frame after a 250 KB join
  keyframe covered the 0.7 s it took to cross, and was almost as large.
- The margin is one floor plus 100 ms, for one loss repair; and the floor leaves out keyframes
  and the deltas sent behind them. Before, the floor came from the join keyframe (0.6–1.3 s),
  the margin grew with it, and the window behaved as in round 2.

A byte budget (no more on the way than the path delivered in the last floor + 100 ms) held
frames back at every loss repair in the same way, and was dropped.

**Headless Chrome** (through the proxy, hotspot, `/viewer?monitor=1`): the page received and
acknowledged frames, with no resyncs. During a window scroll the video delay p50 was 151–174
ms for 20 s. Then the page asked for 6 keyframes in 30 s (`request_keyframe`: more than 8
frames waited in its decoder, which runs in software in headless Chrome), and in the last 5 s
the delay was 754 / 974 ms: nothing is held while a keyframe is on the way, so the deltas sent
behind each one were all sent. After the full-screen scroll was frozen (`SIGSTOP` on the
player), a screenshot of the page matched the clone's own screenshot (0.02 % of the pixels
differ by more than 40 of 255; a scroll offset of one line would differ almost everywhere), so
the last frame was current and not torn. During the full-screen scroll the delay was 0.75–2.2
s; in one such run the page's first round trips came while its 1.8 MB of JavaScript loaded
over the same 3 Mbit/s link, so the floor started at 617 ms and the margin with it. Compressed
frontend files (Z1, round 4) make that load smaller; a cached page does not load it.

### Round 4: side traffic and the browser

2026-10-17, same clone, monitor and client, 30 s per cell, through the proxy on port 9001. Each
cell shows **round 3 → round 4**.

- **Round 3**: the round 3 build (image label `8ab9260-round3`).
- **Round 4**: image label `18b9fb9-round4`. Cursor only to its window (V1), clipboard in
  pieces (V2), keyframe reset in the browser (K3), faster reconnect (D2), compressed frontend
  files (Z1); no resync; a keyframe on the way gets no room of its own; the browser
  acknowledges a frame when its decoder outputs it; the clone daemon ships the newest buffer
  of each batch.

| Link | Motion | fps | Mbit/s mean (peak) | Keyframes | Not encoded | Reconnects | Video delay p50 / p95 (ms) | Echo p50 / p95 (ms) |
|---|---|---|---|---|---|---|---|---|
| lan | still | 0.1 → 0.1 | 0.00 (0.05) → 0.00 (0.05) | 1 → 1 | 0 → 0 | 0 → 0 | 1 / 1 → 1 / 1 | 1 / 1 → 1 / 1 |
| lan | scroll | 50.0 → 50.0 | 0.89 (1.75) → 0.89 (1.73) | 2 → 2 | 0 → 0 | 0 → 0 | 1 / 2 → 1 / 2 | 1 / 21 → 0 / 8 |
| lan | large | 50.0 → 50.0 | 1.61 (4.01) → 1.61 (4.04) | 2 → 2 | 0 → 0 | 0 → 0 | 1 / 2 → 1 / 2 | 1 / 14 → 1 / 11 |
| home | still | 0.1 → 0.1 | 0.07 (2.09) → 0.07 (2.08) | 1 → 1 | 0 → 0 | 0 → 0 | 221 / 221 → 217 / 217 | 51 / 92 → 51 / 92 |
| home | scroll | 49.8 → 49.8 | 0.88 (1.70) → 0.88 (1.75) | 2 → 2 | 0 → 2 | 0 → 0 | 84 / 119 → 78 / 113 | 98 / 99 → 70 / 93 |
| home | large | 49.8 → 49.4 | 1.61 (3.80) → 1.60 (3.62) | 2 → 2 | 3 → 12 | 0 → 0 | 87 / 111 → 81 / 104 | 93 / 111 → 103 / 105 |
| hotspot | still | 0.1 → 0.1 | 0.07 (2.10) → 0.07 (2.10) | 1 → 1 | 0 → 0 | 0 → 0 | 1112 / 1112 → 1383 / 1383 | 194 / 504 → 194 / 603 |
| hotspot | scroll | 42.0 → 43.0 | 0.78 (1.10) → 0.81 (1.32) | 2 → 2 | 176 → 192 | 0 → 0 | 251 / 788 → 275 / 933 | 280 / 540 → 276 / 553 |
| hotspot | large | 27.1 → 35.0 | 1.28 (3.02) → 1.41 (3.42) | 1 → 2 | 478 → 358 | 0 → 0 | 410 / 1715 → 266 / 2015 | 466 / 1519 → 305 / 1438 |

The hotspot cells ran again (fps, video delay p50 / p95 in ms; the round 4 runs include four
of a build that differed only in the version label):

| Motion | Round 3 | Round 4 |
|---|---|---|
| scroll | 46.6, 237 / 835; 45.2, 230 / 750 | 43.6, 236 / 1122; 39.2, 239 / 704 |
| large | 38.1, 244 / 1704; 34.1, 368 / 1513 | 23.4, 388 / 2449; 26.7, 333 / 1837; 34.1, 278 / 1410; 35.0, 255 / 1019; 33.8, 229 / 1871 |

**Headless Chrome** through the proxy, hotspot, full-screen scroll, `/viewer?monitor=1`, 30 s.
A CDP script counted what the page's worker sent. The video delay is the server's p50 of each
5 s line.

| | Keyframe requests | Keyframes in | Video delay p50 per 5 s (ms) | Floor (ms) |
|---|---|---|---|---|
| Round 3 | 6 | 6 | 2221, 2591, 2326, 4649, 4063 | 675 |
| Round 4, run 1 | 0 | 1 | 198, 205, 294, 1502, 381 | 152 |
| Round 4, run 2 | 0 | 2 | 258, 279, 171, 275, 354 | 152 |

**Clipboard** with headless Chrome watching the window scroll through the proxy. A 4.2 MB PNG
of noise (it does not compress). Clone → browser: the clone copies it (`xclip`) at 12 s, and
the page fetches it (CDP turns on focus emulation, so the page acts as the focused window).
Browser → clone: the page offers a 3.8 MB PNG at 12 s (headless Chrome has no clipboard, so the
CDP script replaces the page's `navigator.clipboard.read`), and the clone pastes it.

| Direction | Link | Round 3 | Round 4 |
|---|---|---|---|
| clone → browser | hotspot | One 4.2 MB message blocked the video: no `ack` for 10 s, so the socket closed, 4 times in 70 s; the transfer never ended; 7 keyframe requests. | Done in 40 s, socket open. Video delay p50 350–590 ms, p95 at most 960 ms during the transfer (150–300 ms before). |
| clone → browser | lan | | 4.2 MB in less than 5 s. |
| browser → clone | home | | Upload 5.8 s; the clone pasted it. Video delay p50 53–58 ms. |
| browser → clone | hotspot | | Upload 35 s (the uplink is 1 Mbit/s), socket open, video delay p50 360–470 ms, p95 at most 1.2 s. The clone's paste gave up after its 10 s limit. |

What the numbers show:

- **The LAN and the home link do not change.**
- **A large copy no longer stops the video or closes the socket.** A piece goes only when the
  one before it arrived and the window has room, so it shares the link with the video. The
  price is speed: on the hotspot the clipboard got about 0.8 Mbit/s of the 3.
- **The browser's `bufferedAmount` is not enough for an upload.** The first round 4 build sent
  a piece whenever less than 64 KiB waited unsent in the browser. On the hotspot uplink the
  pieces waited in the kernel and on the link instead, and the window's acknowledgements waited
  behind them: video delay p50 2.2–7.4 s and 0.1–0.3 Mbit/s of video for the 35 s of the
  upload. Since the server acknowledges each piece (`clip_ack`) and the browser waits for it,
  the acknowledgements wait behind one piece at most.
- **Headless Chrome no longer asks for keyframes.** The window acknowledges a frame when its
  (software) decoder has output it, so the server holds frames back for a slow decoder as for
  a slow link, and the decoder's queue stays short. Compressed frontend files (2.7 MB of
  JavaScript is 0.6 MB with brotli) made the first page load short, so the floor no longer
  came from round trips made while the page loaded.
- **No resync.** A build with only the round 3 review's first fix (a window gets room while a
  keyframe is on the way only when at most one frame waits and it is less than 1 s behind)
  ran the whole matrix and headless Chrome with no resync, so round 4 removes it.
- **A keyframe on the way gets no room of its own.** With that fix, the frames sent while a slow
  join keyframe crossed the hotspot link still filled about 1 s of the path: in one run the
  deltas after it took 2 s, the floor rose to 2.3 s, and the video delay p50 was 1.6 s. Now
  those frames follow the 100 ms hold like any other. Over 11 runs of the full-screen scroll on
  the hotspot, that build had 17–36 fps and p50 239–1580 ms; round 4 had 23–35 fps and p50
  229–388 ms in 6 runs.
- The hotspot link changes much from run to run. A swap of only the clone daemon (newest or
  oldest buffer of a batch) on the hotspot full-screen scroll gave 23–36 fps against 27–36 fps
  in 6 runs each: no difference that the bench can show.

### Round 5: automatic bitrate

2026-10-17, same clone, monitor and client, 30 s per cell, through the proxy on port 9001. Each
cell shows **round 4 → round 5**.

- **Round 4**: image label `18b9fb9-round4`.
- **Round 5**: image label `a3a92cf-round5`. The encoder runs VBR with a QP from 25 to 35 and
  no limit (every frame QP 25, as CQP 23/25 before); when the slowest window is shown fewer than
  15 frames per second for 5 s while the screen makes more, the bitrate gets a limit of 70 % of
  what that window delivered (R4, R5; the rule is in
  [PROTOCOL.md](PROTOCOL.md#browser-viewer-protocol-browser--control-server)). Clipboard pieces
  fill 100 ms of the delivered rate in both directions. The server was restarted before each
  link, so no limit carried over from the cells before.
- New motion `photo` (see above), and a column for the bitrate changes (each one is a
  keyframe).

| Link | Motion | fps | Mbit/s mean (peak) | Keyframes | Not encoded | Reconnects | Video delay p50 / p95 (ms) | Echo p50 / p95 (ms) | Bitrate changes |
|---|---|---|---|---|---|---|---|---|---|
| lan | still | 0.1 → 0.1 | 0.02 (0.57) → 0.00 (0.05) | 1 → 1 | 0 → 0 | 0 → 0 | 1 / 1 → 1 / 1 | 1 / 1 → 1 / 1 | 0 |
| lan | scroll | 50.0 → 50.0 | 0.89 (1.75) → 0.89 (1.67) | 2 → 2 | 0 → 0 | 0 → 0 | 1 / 2 → 1 / 2 | 1 / 15 → 1 / 7 | 0 |
| lan | large | 50.0 → 50.1 | 1.61 (4.02) → 1.61 (4.01) | 2 → 2 | 0 → 0 | 0 → 0 | 1 / 2 → 1 / 2 | 1 / 14 → 1 / 10 | 0 |
| lan | photo | 50.0 → 50.0 | 5.85 (15.30) → 5.76 (12.62) | 2 → 2 | 0 → 0 | 0 → 0 | 1 / 2 → 1 / 2 | 1 / 5 → 1 / 11 | 0 |
| home | still | 0.1 → 0.0 | 0.00 (0.05) → 0.00 (0.05) | 1 → 1 | 0 → 0 | 0 → 0 | 95 / 95 → 84 / 84 | 51 / 92 → 51 / 83 | 0 |
| home | scroll | 49.8 → 49.8 | 0.88 (1.74) → 0.88 (1.71) | 2 → 2 | 3 → 2 | 0 → 0 | 82 / 106 → 76 / 103 | 89 / 106 → 75 / 84 | 0 |
| home | large | 49.5 → 49.6 | 1.60 (3.66) → 1.60 (3.56) | 2 → 2 | 11 → 8 | 0 → 0 | 75 / 103 → 76 / 104 | 75 / 92 → 76 / 92 | 0 |
| home | photo | 48.3 → 48.4 | 5.58 (10.35) → 5.60 (10.20) | 2 → 2 | 48 → 42 | 0 → 0 | 81 / 125 → 85 / 139 | 103 / 204 → 107 / 207 | 0 |
| hotspot | still | 0.1 → 0.1 | 0.00 (0.05) → 0.00 (0.05) | 1 → 1 | 0 → 0 | 0 → 0 | 359 / 359 → 154 / 154 | 192 / 293 → 193 / 335 | 0 |
| hotspot | scroll | 38.9 → 42.8 | 0.75 (1.10) → 0.79 (1.18) | 2 → 2 | 232 → 190 | 0 → 0 | 272 / 515 → 265 / 777 | 299 / 522 → 267 / 588 | 0 |
| hotspot | large | 28.4 → 34.1 | 1.05 (2.24) → 1.32 (2.80) | 1 → 1 | 617 → 411 | 0 → 0 | 3326 / 5966 → 339 / 1758 | 1805 / 5204 → 311 / 1629 | 0 |
| hotspot | photo | 5.3 → 5.9 | 1.26 (5.20) → 1.03 (5.48) | 1 → 2 | 736 → 977 | 0 → 0 | 6055 / 8490 → 3767 / 8358 | 5473 / 11012 → 3950 / 10882 | 1 |

The hotspot cells ran again, each on a fresh server (fps, video delay p50 / p95 in ms, bitrate
changes):

| Motion | Round 4 | Round 5 |
|---|---|---|
| large | 23.5, 347 / 1718; 28.9, 378 / 1754; 32.9, 250 / 1383 | 25.4, 300 / 2745, 1; 28.4, 259 / 2089, 1 |
| photo | 6.7, 7689 / 17222; 7.1, 6213 / 16490 | 6.0, 6784 / 13492, 1; 9.7, 4074 / 10508, 1 |

**The hotspot drops to 1 Mbit/s for 20 s** (`down 1mbit` at 15 s, `down 3mbit` at 35 s), one
60 s run on a fresh server. fps is the mean of `rmng-fps`'s seconds 1–15, 16–35 and 41–60.

| Motion | Build | fps before / during / after | Video delay p50 / p95 (ms) | Bitrate changes |
|---|---|---|---|---|
| large | round 4 | 38.6 / 13.3 / 41.3 | 364 / 2149 | – |
| large | round 4, run 2 | 32.4 / 12.6 / 8.1 | 1386 / 2616 | – |
| large | round 5 | 36.3 / 19.0 / 39.1 | 333 / 2924 | 5: no limit → 500 kbit/s 12 s into the drop (9 fps for 5 s, 693 kbit/s delivered); then 625, 781 during the drop and 1171, 1756 after it |
| large | round 5, run 2 | 16.9 / 24.3 / 22.6 | 2468 / 4682 | 0 |
| photo | round 4 | 9.3 / 5.7 / 7.8 | 7310 / 21702 | – |
| photo | round 5 | 9.7 / 7.8 / 16.4 | 6109 / 17103 | 2: no limit → 2359 → 718 kbit/s |

**Clipboard**, as in round 4 (headless Chrome watches the window scroll through the proxy; a
4.2 MB noise PNG down, a 3.8 MB one up):

| Direction | Link | Round 4 | Round 5 |
|---|---|---|---|
| clone → browser | lan | 0.5–1.4 s | 0.4 s |
| browser → clone | lan | | upload 0.4 s, the clone pasted it |
| clone → browser | hotspot | 40 s; video delay p50 350–590 ms, p95 at most 960 ms | 28 s; video delay p50 290–430 ms, p95 at most 690 ms |
| browser → clone | hotspot | upload 35 s; the paste failed | upload 37 s; video delay p50 400–650 ms, p95 at most 950 ms; the paste failed |

**A real Chrome with hardware decoding on the LAN** (headed `google-chrome` 153 on a GNOME
Wayland desktop with a W6800, port 9000 direct, `/viewer?monitor=1`): `SystemInfo` said video
decode is hardware accelerated, and the GPU process's VCN decode time (`drm-engine-dec` in its
`fdinfo`) grew from 1.7 s to 4.4 s during the test, so the decoder was the hardware one. While
the window scrolled, 2–3 frames (35–47 ms) were in its decoder. After each stop (scroll, large,
photo), the next summary line had 0 frames on the way, and in 25 s after each stop the server
logged no stall and the window did not reconnect: the decoder outputs its last frame.

What the numbers show:

- **The LAN and the home link do not change**: the same frame rate, bitrate and delay, and the
  limit never engaged. VBR with no limit gives each frame QP 25; keyframes are QP 25 instead of
  23 (the join keyframe of a still screen is a little smaller).
- **A drop to 1 Mbit/s**: when it engaged (12 s into the drop), the frame rate during the drop
  went from about 13 to about 19 (11–44 fps, most of them above 30, in its last 8 s, against 0–24
in round 4), and after
  the link came back the limit went up in four steps and the frame rate was back at 39. Every
  change is a keyframe: 5 in 60 s, each 100–250 KB of text at QP 25–35. In run 2 the frames
  never stayed under 15 fps for 5 s in a row, so nothing changed; that run started badly (the
  floor came from round trips with a queue in them, see below).
- **On the hotspot, the text cells change little.** A full-screen scroll fits the link at QP 25
  most of the time. The limit engaged in 2 of 3 `large` runs, once each, after seconds in
  which the lossy link delivered little.
- **The picture pan is too much for the hotspot either way**: QP 35 roughly halves the frame
  size (10 KB instead of 20–30 KB), and the frame rate went from 5–7 to 6–10, but the video
  delay stays at seconds. The delay comes from R1, not from the frame size: on this link the
  floor comes from round trips with a queue in them (0.8–2.5 s), R1 then lets that long of
  frames at the capture rate onto the path, and they are 1–3 MB. Holding frames back by bytes
  as well as by time made each held frame larger instead (2 fps, 300–400 KB frames after a
  long hold), so round 5 does not change R1.
- **Clipboard**: on the LAN a 4 MB copy takes 0.4 s each way, as the pieces on the way grow
  with the delivered rate. On the hotspot the download went from 40 s to 28 s with a lower video
  delay. **A paste into the clone of a copy that takes more than 15 s to upload fails**,
  whatever the clone daemon waits: Mutter cancels a clipboard transfer request it has had no
  answer for 15 s (`TRANSFER_REQUEST_CLEANUP_TIMEOUT_MS`), and the bytes that come later fail
  with "Transfer serial … doesn't match any transfer request". On the 1 Mbit/s uplink a 3.8 MB
  image needs 37 s. The clone daemon's own limit is now 15 s to match.

How `vah264enc` on the W6800 (radeonsi, GStreamer 1.28) acts, measured with the same pipeline
on a test program:

- A bitrate change drains the encoder, reconfigures it (6–7 ms; no frame lost) and starts
  again with an IDR at the QP the rate control had reached: 270–550 KB for the picture pan at
  1080p, smaller for text.
- `max-qp` and `min-qp` are kept to. With no limit the rate control sits at `min-qp`.
- **A pipeline that starts in CQP and switches to VBR keeps QP 26 for every frame**, whatever
  the bitrate, also after more changes; one that starts in VBR follows the bitrate. So the
  encoder runs VBR all the time, and "no limit" is 50 Mbit/s.
- CBR pads small frames up to the bitrate (50 Mbit/s CBR gave 40 Mbit/s for a pan that needs
  9), so it is not used.
- Under QP 51 (no `max-qp`), the picture pan needed more bytes than at QP 45: the references
  are so poor that motion search fails. QP 35 is the highest.
- The rate control counts 30 frames per second (the input has no frame rate), so a limit is a
  size per frame of limit / 30.

### Round 6: fixes from review 5, and a clean round-trip floor

2026-10-18, same clone, monitor and client, 30 s per cell, through the proxy on port 9001, a
fresh server before each link. Each cell shows **round 5 → round 6**, both measured the same
night.

- **Round 5**: image label `a3a92cf-round5`.
- **Round 6**: image label `ad10ccc-round6`. Only a second in which the screen made at least
  15 frames counts for the limit; the limit no longer goes away when the screen calms down, it
  goes up by 1.25 only while the frames use 80 % of it, and at 50 Mbit/s it is no limit; one
  tick counts for at most 1.5 s; a low second in which the window's decoder held more than 2
  frames does not count; no delivered rate after 2 s without acknowledged bytes; the floor
  takes only deltas with no queue in front of them; the clipboard window is one floor and
  100 ms. The rules are in [PROTOCOL.md](PROTOCOL.md#browser-viewer-protocol-browser--control-server).

| Link | Motion | fps | Mbit/s mean (peak) | Keyframes | Video delay p50 / p95 (ms) | Bitrate changes |
|---|---|---|---|---|---|---|
| lan | still | 0.1 → 0.0 | 0.00 (0.05) → 0.00 (0.05) | 1 → 1 | 1 / 1 → 1 / 1 | 0 → 0 |
| lan | scroll | 50.0 → 50.0 | 0.89 (1.67) → 0.91 (1.90) | 2 → 2 | 1 / 2 → 1 / 2 | 0 → 0 |
| lan | large | 50.0 → 50.0 | 1.61 (3.99) → 1.61 (3.96) | 2 → 2 | 1 / 2 → 1 / 2 | 0 → 0 |
| lan | photo | 50.0 → 50.0 | 5.77 (12.59) → 5.75 (12.51) | 2 → 2 | 1 / 2 → 1 / 3 | 0 → 0 |
| home | still | 0.1 → 0.1 | 0.00 (0.05) → 0.00 (0.05) | 1 → 1 | 136 / 136 → 51 / 51 | 0 → 0 |
| home | scroll | 49.8 → 49.7 | 0.88 (1.73) → 0.89 (1.38) | 2 → 2 | 85 / 109 → 76 / 116 | 0 → 0 |
| home | large | 49.6 → 49.6 | 1.60 (3.66) → 1.60 (3.55) | 2 → 2 | 84 / 107 → 87 / 111 | 0 → 0 |
| home | photo | 48.4 → 48.3 | 5.59 (9.83) → 5.59 (10.01) | 2 → 2 | 88 / 145 → 75 / 111 | 0 → 0 |
| hotspot | still | 0.0 → 0.0 | 0.00 (0.05) → 0.00 (0.05) | 1 → 1 | 257 / 257 → 255 / 255 | 0 → 0 |
| hotspot | scroll | 37.4 → 43.6 | 0.77 (1.07) → 0.80 (1.23) | 2 → 2 | 264 / 1248 → 242 / 753 | 0 → 0 |
| hotspot | large | 27.9 → 26.4 | 1.24 (3.31) → 1.14 (2.57) | 1 → 1 | 308 / 1589 → 235 / 3567 | 1 → 0 |
| hotspot | photo | 8.0 → 3.5 | 1.04 (3.84) → 1.55 (5.90) | 2 → 2 | 4288 / 9549 → 1950 / 4782 | 1 → 1 |

The hotspot `large` and `photo` cells ran again, each on a fresh server (fps, video delay
p50 / p95 in ms, bitrate changes):

| Motion | Round 5 | Round 6 |
|---|---|---|
| large | 35.7, 242 / 867, 0; 28.3, 256 / 1864, 0; 26.2, 332 / 3122, 1; 32.2, 1248 / 2798, 0; 26.1, 318 / 2564, 0; 34.1, 284 / 1313, 0 | 20.6, 325 / 2715, 2; 20.8, 374 / 3138, 1; 16.8, 1262 / 7187, 2; 16.8, 297 / 4855, 1; 34.2, 273 / 1580, 0; 28.7, 271 / 2270, 1 |
| photo | 5.3, 5322 / 17160, 2; 12.2, 4081 / 10126, 1 | 2.8, 3792 / 5462, 1; 3.2, 2469 / 5508, 1 |

**The hotspot drops to 1 Mbit/s for 20 s**, as in round 5 (fps before / during / after):

| Motion | Build | fps before / during / after | Keyframes | Video delay p50 / p95 (ms) | Bitrate changes |
|---|---|---|---|---|---|
| large | round 5 | 24.1 / 21.1 / 37.6 | 6 | 317 / 2154 | 5: no limit → 500, 625, 937, 1405, 2107 kbit/s |
| large | round 5, run 2 | 42.9 / 20.4 / 33.3 | 5 | 342 / 2653 | 4: no limit → 500, 625, 937, 1405 kbit/s |
| large | round 6 | 39.3 / 18.1 / 33.9 | 4 | 325 / 2475 | 3: no limit → 576, 720, 900 kbit/s |
| large | round 6, run 2 | 37.3 / 20.4 / 40.1 | 4 | 302 / 2109 | 3: no limit → 665, 831, 1038 kbit/s |
| photo | round 5 | 8.1 / 6.6 / 9.6 | 2 | 4385 / 13566 | 2: no limit → 1016 → 500 kbit/s |
| photo | round 6 | 3.7 / 3.0 / 4.7 | 2 | 2222 / 4055 | 1: no limit → 1498 kbit/s |

**Calm, then pan** (new): hotspot, the picture pans 20 s, then only `type` for 15 s, then the
picture pans 20 s again; one 58 s run on a fresh server.

| Build | fps pan / type / pan | Keyframes | Video delay p50 / p95 (ms) | Bitrate changes |
|---|---|---|---|---|
| round 5 | 14.7 / 8.8 / 6.1 | 4 | 2582 / 10427 | 3: no limit → 871; **→ no limit** while typing; → 1840 kbit/s in the second pan |
| round 5, run 2 | 9.1 / 8.0 / 4.8 | 4 | 4647 / 18076 | 3: no limit → 500; **→ no limit** while typing; → 584 kbit/s |
| round 6 | 4.0 / 4.4 / 6.5 | 2 | 2022 / 4600 | 1: no limit → 1790 kbit/s, kept |
| round 6, run 2 | 4.3 / 4.6 / 7.1 | 2 | 1742 / 4814 | 1: no limit → 870 kbit/s, kept |

**A slow or frozen browser on the LAN** (port 9000, no shaping, the picture pans; the server
log's `bitrate:` lines):

| Test | Round 5 | Round 6 |
|---|---|---|
| Headless Chrome (software decoder); its worker paused for 8 s with the debugger, as a frozen tab | 4 changes: no limit → 500 kbit/s at once after the pause ("3 fps for 9 s", one tick of 9 s), then 625, 937, 1405: the limit stays on the LAN | none; after the pause the decoder held 7 frames |
| Headless Chrome (software decoder), 40 s | | none; 50 fps decoded, at most 2 frames in the decoder |
| Headed Chrome (hardware decoder), the window minimized for 30 s (`visibilityState` hidden) | | none; the decoder kept up (20 `ack`s per second, 0 frames in the decoder) |

**Clipboard** with headless Chrome watching the window scroll through the proxy, as in round 5:
4.2 MB down on the hotspot took 40 s (round 5: 28–58 s), with video delay p50 0.3–1.0 s; 3.8 MB
up on the home link 3.7 s, and the clone pasted it; up on the hotspot 37 s, and the paste
failed after Mutter's 15 s, as before. On the hotspot the pieces wait for room as video
frames do, so the larger window does not change the speed.

**The encoder's buffer.** Review 5 found that GStreamer sizes the HRD buffer (`cpb-size`) for
the starting 50 Mbit/s and writes that size (151,514 kbit) back into the property, so it stays
for every later limit; the concern was that nothing then bounds one frame at a low limit.
Measured on CT 101 (the `rmng` container: GStreamer 1.28, radeonsi) with a test program that
feeds the same pipeline 60 frames per second, at a 700 kbit/s limit (23 kbit per frame):

| `target-percentage`, `cpb-size` | Text scroll: kbit/s, largest frame | After the motion stops (same picture again) | Picture pan, QP 35 |
|---|---|---|---|
| 66, 151,514 (round 5) | 1180–1400, 4 KB | 0 KB frames | 7–8 KB frames, also right after 5 s still |
| 66, 150,000 (round 6, fixed) | 1240–1400, 4 KB | 0 KB frames | the same |
| 100, 100,000 (auto) | 1200–1400, 4 KB | 0 KB frames | |
| 100, 350 (half a second) | 1530–1830, 30 KB | 10–55 KB frames (a sharper copy) | the same as 66, 151,514 |
| 66, 531 (the smallest it takes) | 1490–1700, 17 KB | up to 20 KB frames | |
| 66, 5000 | 1290–1490, 9 KB | up to 29 KB frames | |

At 60 frames per second the bitrate is about twice the limit, because the VA rate control
counts 30. The large buffer keeps each frame near its share; a small one makes more bytes,
because the rate control spends a full buffer on a sharper copy of a still picture. So round
6 keeps it, and fixes it in `ENC_TAIL` so that it does not depend on the write-back. A limit
change sets one property, so it is one keyframe.

What the numbers show:

- **The LAN and the home link do not change**, and the limit never engaged there, also with
  a frozen or minimized browser. Round 5 cut the bitrate on the LAN after a frozen tab (one
  9 s tick counted as 9 low seconds) and kept the limit.
- **A calm period keeps the limit.** Round 5 removed it while the user typed and set a new one
  in the next pan: 3 changes and 4 keyframes in 58 s; round 6 has 1 change and 2 keyframes.
  The drop test also needs fewer changes (3 instead of 4–5), as the limit goes up only while
  the frames use it.
- **The floor.** Round 5's hotspot `photo` floor was 0.8–2.6 s. A diagnostic build logged
  why: the first round trip that counted was a lone 297 KB delta, which took 2.7 s to cross,
  and R1 then let 5 s of frames onto the path. The rule from review 5 (at most 100 ms of the
  delivered rate on the way before the delta) was not enough, and as written it also holds
  back good samples: with a round trip over 100 ms the bytes on the way are always more than
  100 ms of the delivered rate while frames flow (on the hotspot `scroll` the first floor came
  only after 8 s, R1 meanwhile let 10 frames per second through, and the bitrate was cut for
  that). So round 6 compares with the path's rate (the highest delivered rate of the last 1–2
  minutes), counts the delta's own bytes, and allows one floor of bytes on the way (a path
  with no queue holds that much). Text floors are now 152–160 ms on the hotspot from the start;
  the picture pan never gets a floor (its frames are larger than 100 ms of the link), so R1
  uses the 100 ms / 1 s rule for it.
- **The picture pan on the hotspot**: the video delay p50 fell from 4.1–5.3 s to 2.0–3.8 s,
  and the p95 from 10–17 s to 5 s, but the frame rate fell from 5–12 to 3–4. It is still too
  much for this link.
- **The full-screen text scroll on the hotspot lost frame rate**: 17–34 fps (mean 23) in 7
  runs against 26–36 (mean 30) for round 5, with more bitrate changes (7 against 2). The floors
  are the same (about 155 ms) and the cuts come after 5–6 s of loss repair, as in round 5; the
  bench did not show why round 6 cuts more often there. This link changes much from run to run
  (round 4: 23–35 fps). A first build with the review's rule as written had 34 fps in 3 runs,
  with floors of 175–270 ms (a looser R1).
