# Viewer test bench: slow links

This bench measures the browser viewer on the LAN and on slow links, before and after a change.
It is row S1 of [WEB-VIEWER-WAN-SCOPE.md](WEB-VIEWER-WAN-SCOPE.md).

## Parts

| Part | Runs on | Does |
|---|---|---|
| [`scripts/wan-shape.sh`](../scripts/wan-shape.sh) | CT 101, as root | Starts a TCP proxy on port 9001 in front of port 9000, as `tailscale serve` is in production. Makes one port (9001 by default, or 9000) look like a slow link with `tc`. Other traffic is not shaped. |
| [`scripts/wan-motion.sh`](../scripts/wan-motion.sh) | the clone, as the clone user | Makes the screen still, or scrolls text in a window, or scrolls text on the full screen. |
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
| `scroll` | A 960×540 window scrolls text at 240 px/s, 60 frames per second. |
| `large` | The same text scrolls on the full monitor: every frame changes. |

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
qdiscs and their counters.

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
