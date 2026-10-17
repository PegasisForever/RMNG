# Viewer test bench: slow links

This bench measures the browser viewer on the LAN and on slow links, before and after a change.
It is row S1 of [WEB-VIEWER-WAN-SCOPE.md](WEB-VIEWER-WAN-SCOPE.md).

## Parts

| Part | Runs on | Does |
|---|---|---|
| [`scripts/wan-shape.sh`](../scripts/wan-shape.sh) | CT 101, as root | Makes TCP port 9000 look like a slow link with `tc`. Other traffic is not shaped. |
| [`scripts/wan-motion.sh`](../scripts/wan-motion.sh) | the clone, as the clone user | Makes the screen still, or scrolls text in a window, or scrolls text on the full screen. |
| `rmng-fps` ([PROTOCOL.md](PROTOCOL.md#browser-viewer-protocol-browser--control-server)) | the bench client | Joins as one viewer window. Prints each second, then for the run: frames, Mbit/s, keyframes, delay, reconnects. |

Profiles (each direction gets half of the round trip):

| Profile | Round trip | Down (server → viewer) | Up | Loss | Jitter | Link buffer |
|---|---|---|---|---|---|---|
| `lan` | no shaping | | | | | |
| `home` | 50 ms | 20 Mbit/s | 10 Mbit/s | 0 | ±2 ms | 200 ms |
| `hotspot` | 150 ms | 3 Mbit/s | 1 Mbit/s | 1 % each way | ±10 ms | 500 ms |

Motion (on monitor 1 of the default two-monitor 1920×1080 layout):

| Motion | Screen |
|---|---|
| `still` | Nothing moves. |
| `scroll` | A 960×540 window scrolls text at 240 px/s, 60 frames per second. |
| `large` | The same text scrolls on the full monitor: every frame changes. |

**Delay** is the time from an `echo` message to its answer, every 200 ms. The answer goes
through the server's queue for this socket and the kernel send buffer, behind the video that
waits there. So the delay is the round trip plus the time the video waits to be sent. It does
not include the encode or the decode.

## Run it

Copy the scripts in once. `$CLONE` is a clone with a desktop.

```sh
SSH="ssh -i ~/.ssh/id_rsa -o IdentityAgent=none -o IdentitiesOnly=yes root@10.0.0.100"
CLONE=pega-speed-source
$SSH 'cat > /tmp/s && pct push 101 /tmp/s /root/wan-shape.sh --perms 755' < scripts/wan-shape.sh
$SSH "cat > /tmp/m && pct push 101 /tmp/m /tmp/wan-motion.sh \
  && pct exec 101 -- docker cp /tmp/wan-motion.sh $CLONE:/tmp/wan-motion.sh" < scripts/wan-motion.sh
cargo build -p control-client --bin rmng-fps
```

Select the clone (the server sends only the selected clone), then run the matrix:

```sh
curl -s -XPOST http://10.0.0.178:9000/api/activate -H 'content-type: application/json' \
  -d "{\"id\":\"$CLONE\"}" >/dev/null
CT="$SSH pct exec 101 --"
for profile in lan home hotspot; do
  $CT /root/wan-shape.sh apply $profile
  for motion in still scroll large; do
    $CT docker exec -u rmng $CLONE bash /tmp/wan-motion.sh $motion
    sleep 3
    echo "$profile $motion: $(target/debug/rmng-fps 10.0.0.178 1 30 | tail -1)"
  done
done
$CT docker exec -u rmng $CLONE bash /tmp/wan-motion.sh still
$CT /root/wan-shape.sh clear
```

Always end with `clear`. `wan-shape.sh show` prints the qdiscs and their counters.

## Results

2026-10-17, CT 101, clone `pega-speed-source`, monitor 1, 30 s per cell. The bench client is a
clone on another CT; it reaches CT 101 over the LAN. Each cell shows **baseline → round 1**.

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
