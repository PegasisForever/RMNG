# media

The media plane library used by [control-server](../control-server/README.md). It turns
raw GPU frames shipped by the clones into **H.264 for the native viewer** (port 1) and
**still images for the desktop MCP** (ports 3/4), and routes input back. This is the
novel, performance-critical part of `RMNG`. Transport is plain framed H.264 over TCP
into a hardware-decode client (no WebRTC).

## Pipeline

```
                         ┌─ continuous (selected clone) → VA-API H.264/monitor ─┐
clone-daemon ──dmabuf────┤                                                       ├─▶ port 1 (viewer)
 (SCM_RIGHTS, per mon)   └─ on demand  (any clone)      → VA-API → PNG/JPEG ─────┴─▶ ports 3/4 (screenshot)
        ▲                                                                            │
        └────────── input (InputMsg) ◀── route from viewer / port-3 agent / port-4 ─┘
                     → clone-daemon socket → Mutter RemoteDesktop
```

## Responsibilities

1. **Ingest** — listen on a host unix socket (bind-mounted into each clone). A clone's
   `clone-daemon` streams, per monitor, a `FrameMsg` (fourcc, modifier, w/h, per-plane
   stride/offset, cursor) + dmabuf fd(s) via `SCM_RIGHTS`. The **selected** clone streams
   continuously; others deliver a single frame on `FrameRequest` (screenshot path).
   - **Back-pressure / sync (the #1 risk)**: the daemon holds the PipeWire buffer until
     this side sends `Ack{monitor, seq}`. Use a deeper ScreenCast pool and explicit-sync
     fences (wait on the render fence before import) to avoid tearing as Mutter recycles
     buffers.
2. **Encode (viewer)** — import each dmabuf into a VA-API surface (reuse the DRM→VAAPI
   device-chain pattern in `../../core/src/rdp/hwdecode.rs`), VPP → NV12, H.264
   low-latency (no B-frames, small/zero GOP, CQP/CBR as tuned in Phase 0),
   **force-IDR on demand**. One encoder per monitor of the selected clone. Emit Annex-B
   access units tagged `{monitor_id, idr}` for the `wire` viewer protocol.
   - Carry forward Phase-0 tuning: `constrained-baseline` isn't required without browsers,
     but keep `aud=true`, `key-int-max` short for fast reconnect, and ample decoder/encoder
     surface headroom (the large-window 60fps cap was decoder surface starvation).
3. **Encode (screenshot)** — import an on-demand dmabuf → VA download/VPP → PNG or JPEG for
   the MCP `Screenshot` tool. Infrequent and request-driven; proven by PoC R3 (cross-
   container dmabuf → JPEG). Optional fallback: have the daemon ship RGB for screenshots if
   GPU readback latency disappoints.
4. **Serve port 1** — multiplex the selected clone's monitor streams over one TCP
   connection to the viewer; honor `RequestKeyframe` (→ force IDR) and a fresh-connect IDR.
   Pace at the sender on a steady clock (Phase-0 R8: damage-driven capture is bursty;
   smooth pacing is the feel-critical task). One connection = one viewer.
5. **Input routing** — a single per-clone input sink: `ViewerInput` (port 1, selected
   clone), port-3 agent actions (that clone), and port-4 actions (named clone) all funnel
   to the target clone's daemon socket. **Coalesce pointer motion** (latest-wins, flush
   ~120 Hz; buttons/keys flush the pending move first) — Phase-0 R8's dominant "latency"
   was injecting every motion event synchronously.
6. **Cursor metadata** — capture is cursor-mode **METADATA** (not embedded), so the video
   has no baked-in cursor. Forward each clone's `CursorMeta` (pos + hotspot + shape-on-change)
   to the viewer so it draws the cursor **client-side** (crisp, low-latency).
7. **Clipboard broker** — hold the current shared selection; fan `ClipboardOffer`s out to the
   viewer **and every connected clone**; serve bytes lazily on `ClipboardRequest` by fetching
   from whichever endpoint copied (rich MIME set). This is what makes remote↔local *and*
   remote↔remote clipboard work; re-bind on `selected` change.
8. **Switching** — on `state.selected` change: stop the old continuous feed + encoders,
   start the new clone's feed, (re)negotiate the viewer's monitor set, force IDR.

## Dependencies

`ffmpeg-sys-the-third` / `ffmpeg-next` (VA-API H.264 encode) — or `cros-libva` for finer
dmabuf-import / force-IDR control; `image` (PNG/JPEG for screenshots); `tokio` (sockets +
tasks); `nix`/`sendfd` (SCM_RIGHTS fd passing); `wire`. No WebRTC/SRTP/DTLS dependency,
which also removes the crypto-provider clash the old plan worried about.

## Tests

- **dmabuf → encode**: import a daemon-shipped dmabuf and dump a valid `.h264` (verify with
  ffprobe/decode) — the foundational check.
- **dmabuf → PNG**: a single on-demand frame round-trips to a correct screenshot.
- **sync**: sustained capture+encode with no tearing under back-pressure + explicit-sync.
- **viewer protocol**: frame the multiplexed stream to a stub client; `RequestKeyframe`
  forces an IDR; one connection only.
- **multi-monitor + switch**: N encoders; switch `selected` → re-target + IDR within a
  frame or two.
- **input**: coalesced pointer motion never backlogs; a click lands at the latest position.

## Risks

- VA-API import of Mutter's AMD dmabuf **modifier** (tiled/CCS) — PoC R4 imported the tiled
  modifier directly (no blit); validate per driver version.
- Sender-side **pacing** to match RFX feel (Phase-0 R8) — pace on a steady clock, give the
  decoder/encoder surface headroom.
