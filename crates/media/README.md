# media

The media plane library used by [control-server](../control-server/README.md). It turns
raw GPU frames shipped by the clones into **H.264 for the browser viewer** and
**still images for the desktop MCP**, and routes input back. This is the
novel, performance-critical part of `RMNG`. The control-server sends the H.264 to the
browser over a WebSocket on port 9000, where Chrome decodes it with WebCodecs (no WebRTC).

## Pipeline

```
                         ┌─ continuous (selected clone) → VA-API H.264/monitor ─┐
clone-daemon ──dmabuf────┤                                                       ├─▶ :9000 /api/viewer
 (SCM_RIGHTS, per mon)   └─ on demand  (any clone)      → VA-API → JPEG ─────────┴─▶ MCP screenshot
        ▲                                                                            │
        └────────── input (InputMsg) ◀── route from viewer / MCP actions ────────────┘
                     → clone-daemon socket → Mutter RemoteDesktop
```

## Responsibilities

1. **Ingest** — listen on a host unix socket (bind-mounted into each clone). A clone's
   `clone-daemon` streams, per monitor, a `FrameMsg` (fourcc, modifier, w/h, per-plane
   stride/offset, cursor) + dmabuf fd(s) via `SCM_RIGHTS`. The server turns a clone's
   capture on (`Capture{active}`) only while a viewer watches it.
   - **Back-pressure / sync (the #1 risk)**: the daemon holds the PipeWire buffer until
     this side sends `Ack{monitor, seq}`. Use a deeper ScreenCast pool and explicit-sync
     fences (wait on the render fence before import) to avoid tearing as Mutter recycles
     buffers.
2. **Encode (viewer)** — import each dmabuf into a VA-API surface, VPP → NV12, H.264
   low-latency (no B-frames, small/zero GOP, CQP/CBR as tuned in Phase 0),
   **force-IDR on demand**. One encoder per monitor of the selected clone. Emit Annex-B
   access units tagged `{monitor_id, idr}` for the browser viewer protocol (`wire::browser`).
   - The stream is `constrained-baseline` with `aud=true`, `key-int-max` at its highest (1024: keyframes come on demand, a reconnect asks for one), and ample decoder/encoder
     surface headroom (the large-window 60fps cap was decoder surface starvation).
   - **Colour**: pin `colorimetry=2:3:0:0` on the NV12 caps (limited range, BT.709 matrix,
     transfer left unset). The encoder writes no VUI colour description, so the viewer has to
     assume a matrix, and an unpinned one follows the frame size. Naming a transfer here would
     make `vapostproc` convert the samples; the browser viewer supplies the missing sRGB
     transfer in its decoder config.
3. **Encode (screenshot)** — import an on-demand dmabuf → VA download/VPP → JPEG for
   the MCP `Screenshot` tool. The clone-daemon calls this inside the clone. Infrequent and request-driven; proven by PoC R3 (cross-
   container dmabuf → JPEG). Optional fallback: have the daemon ship RGB for screenshots if
   GPU readback latency disappoints.
   - **Colour**: convert to `colorimetry=1:4:0:0` (full range, BT.601) before `jpegenc`, the
     JFIF convention. A JPEG carries no colour description, so a reader that gets limited-range
     BT.709 lifts blacks ~15, crushes whites ~20, and drains ~37 out of saturated red.
4. **Serve the viewer** — the control-server (`mediaplane`, `webviewer`) sends each monitor's
   stream to the browser windows of that monitor, one WebSocket per window; it honors
   `RequestKeyframe` (→ force IDR) and a fresh-connect IDR.
5. **Input routing** — a single per-clone input sink: viewer `InputMsg` (selected
   clone) and MCP actions (`rmng desktop`, the `/api/hosts/:id/mcp` proxy) all funnel
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

GStreamer (`gstreamer`, `-app`, `-base`, `-video`, `-allocators`: VA-API H.264 and JPEG
encode), `tokio` (sockets + tasks), `nix`/`libc` (SCM_RIGHTS fd passing), `wire`. No
WebRTC dependency.

## Tests

- **dmabuf → encode**: import a daemon-shipped dmabuf and dump a valid `.h264` (verify with
  ffprobe/decode) — the foundational check.
- **dmabuf → JPEG**: a single on-demand frame round-trips to a correct screenshot.
- **sync**: sustained capture+encode with no tearing under back-pressure + explicit-sync.
- **viewer protocol**: `rmng-fps` (control-client) joins `/api/viewer` and counts the frames
  that arrive.
- **multi-monitor + switch**: N encoders; switch `selected` → re-target + IDR within a
  frame or two.
- **input**: coalesced pointer motion never backlogs; a click lands at the latest position.

## Risks

- VA-API import of Mutter's AMD dmabuf **modifier** (tiled/CCS) — PoC R4 imported the tiled
  modifier directly (no blit); validate per driver version.
- Sender-side **pacing** to match RFX feel (Phase-0 R8) — pace on a steady clock, give the
  decoder/encoder surface headroom.
