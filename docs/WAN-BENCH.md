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
acknowledged frames, the floor, how far behind the viewer is, and resyncs.

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
