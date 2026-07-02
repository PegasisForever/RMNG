# Task: zero-copy GL AVC444 **pack** (encode side)

Self-contained brief. You implement the **encoder** half of RMNG's full-chroma (4:4:4) video
path as a **zero-copy GPU pipeline**. A separate agent implements the matching **decoder**
([ZEROCOPY_DECODE.md](ZEROCOPY_DECODE.md)). You do not need to read that doc — the two halves
only share one contract: the byte layout in [crates/wire/src/avc444.rs](crates/wire/src/avc444.rs)
(do **not** change it).

---

## 1. Background

RMNG is a Rust cloud-desktop streamer. A clone captures its desktop to a GPU **dmabuf**
(fourcc `AR24` = ARGB8888, AMD tiled modifier `0x0200000020801b03`) and ships the FD to the
**control-server**, which H.264-encodes it (VA-API on an **AMD Radeon Pro W6800**, RDNA2/VCN3)
and streams Annex-B AUs to a viewer over TCP.

The W6800 hardware encoder is **4:2:0 only**. To deliver full chroma (4:4:4 — crisp colored
text), RMNG uses the **RDP AVC444 trick in a single double-height stream**: pack the image's
luma + a base chroma into the **top** half of a `W×2H` NV12 frame, and the *dropped* chroma into
the **bottom** half, then H.264-encode that one `W×2H` frame. The viewer reassembles 4:4:4.

This is gated by a server-wide config toggle `chroma: Yuv420|Yuv444` (default `Yuv420`;
env override `RMNG_CHROMA=yuv420|yuv444`). In `Yuv444` the encoder emits the `W×2H` stream.

### What already exists (working, tested)
- **The packing contract** — [crates/wire/src/avc444.rs](crates/wire/src/avc444.rs): pure
  `pack_y444_to_stacked_nv12()` / `unpack_*()` + `ycbcr_to_rgb_bt601()`, round-trip unit-tested
  (lossless). **This is the exact byte layout your GL shader must reproduce.** Read its module
  doc comment — it has the full layout diagram.
- **A CPU implementation you are replacing** — [crates/media/src/encode.rs](crates/media/src/encode.rs)
  `Encoder` (`Yuv444` branch): two pipelines bridged by a CPU pack
  (`appsrc(dmabuf) ! vapostproc ! Y444 ! appsink` → `avc444::pack` → `appsrc(NV12 W×2H) ! vah264enc`).
  It works and is hardware-validated (round-trip = 0.00 chroma error via
  [crates/media/src/bin/avc444_e2e.rs](crates/media/src/bin/avc444_e2e.rs)), **but it costs ~11 MB
  GPU→CPU download + ~22 MB CPU shuffle + ~11 MB CPU→GPU upload per frame per monitor.** Your job
  is to eliminate all of that.
- `media::init()` ([crates/media/src/lib.rs](crates/media/src/lib.rs)) already sets the headless
  GL env (`GST_GL_GBM_DRM_DEVICE=/dev/dri/renderD128`, `GST_GL_WINDOW=gbm`, `GST_GL_PLATFORM=egl`)
  — **required**, because the control-server is headless (no display, render-node-only) and GstGL's
  GBM backend can't autodetect a device otherwise.
- `gstreamer-gl = { version = "0.23", features = ["v1_24"] }` is already a dependency of the
  `media` crate.

---

## 2. The shared contract (do not change): stacked `W×2H` NV12 layout

From `wire::avc444`. For a chroma plane `C` (`w×h`), define phase quadrants
`Cij(x,y) = C[2x+i, 2y+j]`, each `w/2 × h/2`. The stacked frame (`W=w`, `H=h`):

```
LUMA plane (W × 2H):
  rows [0   .. H )  = Y                                  (full-res image luma)
  rows [H   .. 2H)  = aux luma, 2×2 tiling of (w/2×h/2):  ┌──────┬──────┐
                                                          │ Cb01 │ Cb10 │  rows H .. H+h/2
                                                          ├──────┼──────┤
                                                          │ Cb11 │ Cr01 │  rows H+h/2 .. 2H
                                                          └──────┴──────┘
CHROMA plane (W × H, interleaved U,V):
  rows [0   .. H/2) = main: U = Cb00, V = Cr00
  rows [H/2 .. H )  = aux:  U = Cr10, V = Cr11
```

Colorimetry: **BT.601 limited range** (matches `ycbcr_to_rgb_bt601` / the decoder). Your pack
shader does RGB→YCbCr in BT.601 limited range. The main view's chroma here is the `(0,0)` phase
*point sample* (not an average) — keep it that way so it's an exact inverse of the decoder gather.

**Validation rule:** the bytes your GL pack writes into the `W×2H` NV12 must be identical (modulo
H.264 quantization) to what `avc444::pack_y444_to_stacked_nv12` produces for the same source.
That is how you check correctness without the decoder agent.

---

## 3. The proven zero-copy pipeline (hardware-verified on the W6800)

I verified these on CT 106 with `gst-launch` (headless, GBM env set):

- ✅ `vah264enc` encodes `2560×2880` (stacked 1440p) fine.
- ✅ **`<GL> ! gldownload ! "video/x-raw(memory:DMABuf)" ! vapostproc ! "video/x-raw(memory:VAMemory),format=NV12" ! vah264enc`** runs end-to-end; the `gldownload→vapostproc` link negotiates `memory:DMABuf` (zero-copy EGL dmabuf export + VA import, GPU-only).
- ❌ **Direct `gldownload ! vah264enc` falls back to system memory** (the encoder's dmabuf import only advertised the AMD-tiled modifier; GL exports a different one). **You must route GL→VA through `vapostproc`** — it normalizes the dmabuf to a VA surface on-GPU.

**Target pipeline (replaces the `Yuv444` branch of `Encoder`):**
```
appsrc(dmabuf AR24)
  ! glupload                                  # EGL dmabuf import → GLMemory  [zero-copy*]
  ! glcolorconvert                            # → RGBA GLMemory (GPU)
  ! rmngavc444pack                            # YOUR element: RGBA W×H → NV12 W×2H GLMemory (GPU)
  ! gldownload ! "video/x-raw(memory:DMABuf)" # EGL dmabuf export            [zero-copy]
  ! vapostproc ! "video/x-raw(memory:VAMemory),format=NV12"   # dmabuf→VA surface (GPU)
  ! vah264enc aud=true b-frames=0 ref-frames=1 key-int-max=30 rate-control=cqp qpi=23 qpp=25
  ! video/x-h264,profile=constrained-baseline ! h264parse config-interval=-1
  ! video/x-h264,stream-format=byte-stream,alignment=au ! appsink
```
Only the compressed AU (~KB) leaves the GPU. Per-frame full-frame host traffic → **0**.

**`*` The input-import link — VERIFIED** (2026-06-28, CT 106 / W6800): `glupload` imports an
`AR24:0x0200000020801b03` dmabuf — exactly the capture fourcc + AMD tiled modifier — as a GL texture
**zero-copy**. Proven with
`videotestsrc ! BGRA ! vapostproc ! "video/x-raw(memory:DMABuf)" ! glupload ! "video/x-raw(memory:GLMemory)"`,
which negotiated `format=DMA_DRM, drm-format=AR24:0x0200000020801b03` → `GLMemory RGBA texture-target=2D`
and ran clean. (An earlier "failure" was a caps-string bug — forcing `format=NV12` on a
`memory:DMABuf` cap. Modern GStreamer negotiates `format=DMA_DRM` + a `drm-format` field; **let the
format negotiate**, don't pin it.) The only residual variable is the *allocator*: that probe's dmabuf
came from VA, the live one comes from the clone's compositor — same fourcc/modifier/GPU/driver, the
standard Mesa EGL compositor-import case. Treat the input side as **unblocked**; just do a final live
confirm (§6).

---

## 4. The element: `rmngavc444pack` (custom GLFilter)

Use `gstreamer_gl::subclass` (`GLFilterImpl` : `GLBaseFilterImpl` : `BaseTransformImpl` :
`ElementImpl` : `GstObjectImpl` : `ObjectImpl` : `ObjectSubclass`). Put it in a new file
`crates/media/src/glpack.rs`; register the type in-process (`Element::register(None, "rmngavc444pack", Rank::NONE, ...)`) before building the pipeline, then `ElementFactory::make("rmngavc444pack")` — or construct via `glib::Object::new` and add to the bin.

Key API (in `~/.cargo/registry/src/*/gstreamer-gl-0.23.7/src/` — read it):
- `GLFilterImpl`: `const MODE: GLFilterMode = GLFilterMode::Buffer;` (NV12 output is multi-plane),
  `const ADD_RGBA_PAD_TEMPLATES = false;` (you define custom caps), and impl `filter(&self, in: &Buffer, out: &Buffer)`, `transform_internal_caps(...)`, `set_caps(in, out)`.
- Pad templates (via `ElementImpl::pad_templates`): **sink** `video/x-raw(memory:GLMemory),format=RGBA`; **src** `video/x-raw(memory:GLMemory),format=NV12`.
- Size relation in `transform_internal_caps`: **src height = 2 × sink height**, src width = sink width. (Sink = RGBA `W×H`, src = NV12 `W×2H`.)
- Rendering: `GLFilterExt::render_to_target_with_shader(input: &GLMemory, output: &GLMemory, &GLShader)` renders a fullscreen quad over `output`, sampling `input` (default vertex stage gives `v_texcoord`; sampler uniform is `tex`). **NV12 output is 2 GLMemory planes** — get them with `out.peek_memory(0)` (Y, R8, `W×2H`) and `peek_memory(1)` (UV, RG8, `W×H`), downcast to `GLMemory`, and call `render_to_target_with_shader` **once per plane** with a plane-specific shader and the single RGBA input GLMemory. This keeps you on the single-input helper (no manual FBO needed).
- Build shaders with `GLShader::new(ctx)` + `GLSLStage::new_default_vertex(ctx)` + a custom fragment `GLSLStage` (check the crate for `with_strings`/`new_with_strings`; else `compile_attach_stage`), then `link()`. Get the context via `GLBaseFilterExt::gl_context()` (compile lazily in `init_fbo`/`gl_start`, store in a `Mutex<Option<...>>`). Pass `W`,`H` as uniforms (`set_uniform_1f`).

### Shader math (port from `wire::avc444::pack_*`)
Two fragments, each rendering one output plane; both read the RGBA input and do BT.601-limited
RGB→YCbCr inline. For output pixel `(px, r)`:

- **Y-plane** (output `W×2H`): if `r < H` → `Y(px, r)` (main luma). If `r ≥ H` → aux luma tiling:
  with `ay = r - H`, block = which of `[Cb01|Cb10 ; Cb11|Cr01]` `(px, ay)` lands in → sample the
  source at the corresponding `2x+i, 2y+j` position and output that chroma channel. (Mirror the
  index arithmetic in `pack_y444_to_stacked_nv12`.)
- **UV-plane** (output `W×H` interleaved): chroma rows `[0,H/2)` → main `(U=Cb00, V=Cr00)`; rows
  `[H/2,H)` → aux `(U=Cr10, V=Cr11)`. Even output column → U, odd → V (NV12 interleave).

The exact source-pixel indices for every quadrant are in `pack_y444_to_stacked_nv12`; transcribe
them. **Sample with texel-center offsets** (`(p + 0.5)/dim`) and `floor()` integer math.

---

## 5. Integration

In [crates/media/src/encode.rs](crates/media/src/encode.rs), replace the `Yuv444` branch's
ingest+pack (the `vapostproc ! Y444 ! appsink` + `pack_and_push` + separate encode pipeline) with
the **single** target pipeline from §3. Keep:
- `Encoder::new(chroma, on_au)` signature, `push(fd, fourcc, modifier, w, h)`, `force_idr()`, and
  the `attach_au_sink` AU callback (unchanged — still one Annex-B AU per source frame).
- The `Yuv420` branch untouched.
- `media::init()`'s GL env (already set).

The `force_idr` event must reach `vah264enc` (now in the single pipeline again — simpler than the
CPU two-pipeline version).

---

## 6. Build, test, validate (all on CT 106 — the W6800 box)

The control-server/encoder only runs on the W6800. You cannot test encode locally (Intel/no VA-AVC).

**Reach CT 106** (warm cargo cache at `/root/RMNG`, `cargo 1.96`):
```sh
# sync source (host key for the CT isn't trusted; go via the Proxmox host + pct exec stdin):
tar c --exclude=target --exclude=.git --exclude=frontend/node_modules . \
  | ssh root@10.0.0.100 'pct exec 106 -- bash -c "cd /root/RMNG && tar x"'
# build + run on the CT (GL env is set by media::init, but gst-launch tests need it explicit):
ssh root@10.0.0.100 'pct exec 106 -- bash -lc "cd /root/RMNG && cargo build -p media --bin avc444_e2e"'
```
Run binaries with `GST_GL_GBM_DRM_DEVICE=/dev/dri/renderD128 GST_GL_WINDOW=gbm GST_GL_PLATFORM=egl`
for any `gst-launch` experiments (the in-process `media::init` already sets them for the binaries).

**Validation ladder:**
1. **`glupload` source import — already verified** (§3 `*`): the `AR24:0x0200000020801b03` import is
   proven zero-copy on this hardware, so this is no longer a gating blocker. Just do a final live
   confirm: bring up a clone feeding the control-server with `RMNG_CHROMA=yuv444` and watch the
   `decode[]`/encoder logs for negotiation errors.
2. **Pixel correctness vs the oracle**: extend [crates/media/src/bin/avc444_e2e.rs](crates/media/src/bin/avc444_e2e.rs)
   (it already does pack→`vah264enc`→`vah264dec`→unpack and reports chroma error + dumps PNGs).
   Add a path that runs *your GL pack* instead of `avc444::pack` and assert the same ~0 chroma
   error. The CPU `avc444` functions are your ground truth.
3. **Zero-copy proof**: run the pipeline with `GST_DEBUG=GST_CAPS:4` and confirm the
   `gldownload→vapostproc` link shows `memory:DMABuf` (not plain `video/x-raw`), and that no element
   reports a sysmem copy.
4. **Throughput**: confirm sustained fps at 2×1440p (the default dual-monitor layout) with no
   per-frame host transfers.

---

## 7. Definition of done
- `RMNG_CHROMA=yuv444` encode runs the §3 zero-copy pipeline; `Yuv420` unchanged.
- GL pack output matches `wire::avc444` (oracle) within H.264 tolerance (avc444_e2e ≈ 0 chroma error).
- `gldownload→vapostproc` confirmed `memory:DMABuf`; no full-frame host copies per frame.
- `glupload` importing the AMD-tiled capture dmabuf — already verified zero-copy (§3 `*`); confirm once live.
- `crates/media/src/encode.rs` `Yuv420` path and the `Encoder` public API unchanged; workspace builds; `cargo test -p wire` green (you must not break the `avc444` contract).

## 8. Gotchas
- **Headless GL needs the GBM env** — already in `media::init`; don't remove it.
- **Never go GL→`vah264enc` directly** — always via `vapostproc` (proven).
- **Don't `glcolorconvert` NV12→RGBA anywhere on the chroma data** — it would 4:2:0-subsample and destroy the packing. Your element reads RGBA (the *image*) and writes NV12 (the *packed* frame) itself.
- **Don't change `crates/wire/src/avc444.rs`** — it's the contract with the decoder agent.
- Widths/heights are even and (for the default) multiples of 64; you may assume even `W,H`.

---

## 9. Prior art & validated fallback topology (gnome-remote-desktop)

The §3 pipeline is the **primary** design. This section records the prior art it's based on and a
**validated fallback** topology — read it only if §3 hits trouble; you do not need it to start.

### What the references do
- **FreeRDP** (`libfreerdp/primitives/prim_YUV.c`): `general_YUV444SplitToYUV420` /
  `general_YUV420CombineToYUV444` — the AVC444 split/combine as **CPU** SIMD (SSE4.1/NEON) over
  planar YUV. This is the *layout oracle* `wire::avc444` is modeled on. **Not** zero-copy (CPU).
- **IronRDP** (`crates/ironrdp-glutin-renderer/shaders/avc444.frag`): reconstruction in a **GPU
  fragment shader** (BT.601 limited-range), but fed by OpenH264 **software** decode + a per-frame
  `glTexImage2D` upload — so GPU reconstruction, CPU decode/upload. Relevant to the *decode* agent,
  not here. (`avc444v2.frag` packs the aux view *side-by-side* — a horizontal analog of our vertical
  `W×2H` stack.)
- **gnome-remote-desktop** (MR !294, "zero-copy rendering using Vulkan and VAAPI"): the only
  reference that is **genuinely zero-copy on our exact hardware class** (AMD + VA-API). It is **raw
  libva + raw Vulkan, not GStreamer.** Its topology **inverts buffer ownership** vs §3:
  ```
  VA ALLOCATES the NV12 encode surface            (vaCreateSurfaces, YUV420)
    → exports it as per-plane dma-buf             (vaExportSurfaceHandle,
                                                   VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                                                   SEPARATE_LAYERS | WRITE_ONLY)
    → GPU imports each plane as an R8/RG8 image    (grd_vk_dma_buf_image_new, VK_FORMAT_R8_UNORM)
    → GPU shaders do RGB→YCbCr + AVC444 aux,        writing DIRECTLY into VA's memory
    → VA encodes in place                          — no download, no second normalization pass
  ```
  (g-r-d uses standard RDP **two-stream** AVC444 — two separate surfaces — not our single stacked
  `W×2H` frame; that difference is orthogonal to the zero-copy mechanism.)

### Why §3 is primary, not this
g-r-d's inversion only eliminates §3's `gldownload` + `vapostproc` — and **both are already
zero-copy here** (verified 2026-06-28: §3's chain negotiates `memory:DMABuf`/`DMA_DRM` end-to-end
with the `0x0200000020801b03` modifier, no host copy, no sysmem fallback). In GStreamer, `gldownload
→ vapostproc → VAMemory` *is* the idiomatic GstGL↔GstVA interop. g-r-d's "GPU writes into the
encoder's own surface" pattern is idiomatic only outside GStreamer, where it has full allocator
control. So adopting it here buys ~nothing (the eliminated steps cost no host copies) at the price of
a non-idiomatic custom element. Don't lead with it.

### The fallback — only if §3's `gldownload → vapostproc` fails on some GPU/driver
If on a future target that link **falls back to system memory** (it didn't on the W6800), or
profiling shows `vapostproc` doing a real VPP **blit** instead of a zero-copy surface wrap, port
g-r-d's ownership inversion into the GStreamer element:
1. Let **`vah264enc`** (its `GstVaAllocator` pool) own the `W×2H` NV12 buffers.
2. In your element, **import each plane's dmabuf as a `GstGLMemoryEGL` FBO render target** and render
   the pack shaders **in place**, then push straight to the encoder — dropping `gldownload` +
   `vapostproc` entirely.
3. **The hard link is already de-risked:** the VA→GL dmabuf import direction is **proven** here —
   `vapostproc ! "video/x-raw(memory:DMABuf)" ! glupload` imported a VA-exported
   `AR24:0x0200000020801b03` surface as a GL texture zero-copy (2026-06-28, CT 106). Rendering *into*
   an imported `GstGLMemoryEGL` (vs sampling from it) is the extra step to build.

Vulkan/RADV **is** installed on the W6800 (`libvulkan_radeon.so`, `radeon_icd.json`), so a literal
raw-libva+Vulkan reimplementation à la g-r-d is also possible — but that abandons GStreamer for the
encode path and breaks consistency with the rest of the `media` crate; treat it as a last resort.
