# Task: zero-copy GL AVC444 **unpack** (decode side)

Self-contained brief. You implement the **viewer/decoder** half of RMNG's full-chroma (4:4:4)
video path as a **zero-copy GPU pipeline**. A separate agent implements the matching **encoder**
([ZEROCOPY_ENCODE.md](ZEROCOPY_ENCODE.md)). You do not need to read that doc — the two halves only
share one contract: the byte layout in [crates/wire/src/avc444.rs](crates/wire/src/avc444.rs)
(do **not** change it).

---

## 1. Background

RMNG is a Rust cloud-desktop streamer. The **control-server** (on an AMD W6800) H.264-encodes a
clone's desktop and streams Annex-B AUs to the native **viewer** ([crates/viewer/src/main.rs](crates/viewer/src/main.rs))
over TCP (port 1). The viewer is a GTK4 app that VA-API-decodes and shows one window per monitor
via `vah264dec ! glupload ! gtk4paintablesink` (zero-copy GL paintable). The viewer runs on the
operator's machine, which **has a display and a working GL context** (unlike the headless server).

For full chroma (4:4:4 — crisp colored text) RMNG uses the **RDP AVC444 trick in a single
double-height stream**: the encoder packs the image's luma + base chroma into the **top** half of a
`W×2H` NV12 frame and the *dropped* chroma into the **bottom** half, H.264-encodes that one `W×2H`
frame, and the viewer **reassembles 4:4:4**. So in `Yuv444` mode the viewer receives a stream that
decodes to `W×2H` NV12 and must reconstruct a `W×H` RGB image.

Mode is announced by the server at connect via a **tag-4 handshake** (already implemented): the
viewer's read loop sets a process-global `VIEWER_CHROMA` atomic (`0`=Yuv420, `1`=Yuv444) **before
the first AU**, and `make_decoder` branches on it.

### What already exists (working, tested)
- **The packing contract** — [crates/wire/src/avc444.rs](crates/wire/src/avc444.rs): pure
  `unpack_stacked_nv12_to_rgba(luma, luma_stride, chroma, chroma_stride, w, h) -> RGBA` and
  `ycbcr_to_rgb_bt601()`, round-trip unit-tested against the packer (lossless). **This is the exact
  reconstruction your GL shader must reproduce.** Read its module doc — full layout diagram + the
  gather indices.
- **A CPU implementation you are replacing** — [crates/viewer/src/main.rs](crates/viewer/src/main.rs)
  `make_decoder_yuv444()`: two pipelines bridged by a CPU unpack
  (`appsrc(h264) ! h264parse ! vah264dec ! videoconvert ! NV12 ! appsink` → `avc444::unpack_*_rgba`
  → `appsrc(RGBA) ! gtk4paintablesink`). It works, but costs **~11 MB GPU→CPU download +
  per-pixel YCbCr→RGB on the CPU over W·H px + ~15 MB CPU→GPU upload per frame** — won't sustain
  1440p60. Your job is to do the reconstruction **on the GPU**, eliminating all of that.
- `make_decoder(monitor_id)` returns `(AppSrc, gdk::Paintable)`; the rest of the viewer (letterbox,
  cursor overlay, fps — all keyed on the `W×H` paintable) is mode-agnostic. **Keep that interface.**
- `gstreamer-gl = { version = "0.23", features = ["v1_24"] }` is already a dependency of the
  `viewer` crate.

---

## 2. The shared contract (do not change): stacked `W×2H` NV12 → `W×H` RGB

From `wire::avc444`. Phase quadrants `Cij(x,y) = C[2x+i, 2y+j]` (each `w/2×h/2`). The encoder lays
out the decoded `W×2H` NV12 (`W=w`, `H=h`) as:

```
LUMA plane (W × 2H):
  rows [0   .. H )  = Y                                  (full-res image luma)
  rows [H   .. 2H)  = aux luma, 2×2 tiling:               ┌──────┬──────┐
                                                          │ Cb01 │ Cb10 │  rows H .. H+h/2
                                                          ├──────┼──────┤
                                                          │ Cb11 │ Cr01 │  rows H+h/2 .. 2H
                                                          └──────┴──────┘
CHROMA plane (W × H, interleaved U,V):
  rows [0   .. H/2) = main: U = Cb00, V = Cr00
  rows [H/2 .. H )  = aux:  U = Cr10, V = Cr11
```

**Reconstruction** for output pixel `(x,y)` (`x∈[0,W)`, `y∈[0,H)`), with `i=x&1, j=y&1, xc=x>>1,
yc=y>>1`:
- `Y  = luma[x, y]`            (main luma, top region)
- `Cb`: `(0,0)`→`chroma U @(xc,yc)` · `(1,0)`→`luma[xc, H+yc]` · `(0,1)`→`luma[W/2+xc, H+yc]` · `(1,1)`→`luma[xc, H+H/2+yc]`
- `Cr`: `(0,0)`→`chroma V @(xc,yc)` · `(1,0)`→`luma[W/2+xc, H+H/2+yc]` · `(0,1)`→`chroma U @(xc, H/2+yc)` · `(1,1)`→`chroma V @(xc, H/2+yc)`
- `RGB = ycbcr_to_rgb_bt601(Y, Cb, Cr)` — **BT.601 limited range** (must match the encoder).

The exact indices + the matrix are in `unpack_stacked_nv12_to_rgba` / `ycbcr_to_rgb_bt601`;
transcribe them into GLSL. **Validation rule:** your GL output must equal `unpack_stacked_nv12_to_rgba`
on the same decoded frame (the CPU function is your ground truth — see §6).

---

## 3. The target zero-copy pipeline

The viewer has a GL context, so the *whole* decode stays in VRAM — no `vapostproc` bridge needed
(it ends at the GL paintable):
```
appsrc(h264) ! h264parse ! vah264dec ! glupload ! rmngavc444unpack ! gtk4paintablesink
```
- `vah264dec ! glupload` → **NV12 GLMemory `W×2H`** (2 textures: Y as R8, UV as RG8). This is the
  existing zero-copy path used by the `Yuv420` decoder.
- `rmngavc444unpack` (YOUR element): NV12 `W×2H` → RGBA `W×H`, doing the §2 gather + BT.601 matrix
  on the GPU.
- `gtk4paintablesink` displays the RGBA GL texture (zero-copy). Intrinsic size `W×H` → letterbox /
  cursor / fps logic all keep working unchanged.

Net per frame: **0 full-frame host copies** (today's CPU path costs ~26 MB transfer + a full-frame
matrix pass on the CPU).

---

## 4. The element: `rmngavc444unpack` (custom GLFilter)

Use `gstreamer_gl::subclass` (`GLFilterImpl` : `GLBaseFilterImpl` : `BaseTransformImpl` :
`ElementImpl` : ...). New file `crates/viewer/src/glunpack.rs`; register in-process
(`Element::register(None, "rmngavc444unpack", Rank::NONE, ...)`) before building the pipeline.

Key API (in `~/.cargo/registry/src/*/gstreamer-gl-0.23.7/src/` — read it):
- `GLFilterImpl`: `const MODE = GLFilterMode::Buffer;` (NV12 input is **2 planes**, so you need
  `filter(&self, in: &Buffer, out: &Buffer)`, not the single-texture helper),
  `const ADD_RGBA_PAD_TEMPLATES = false;`, plus `transform_internal_caps` + `set_caps`.
- Pad templates: **sink** `video/x-raw(memory:GLMemory),format=NV12`; **src**
  `video/x-raw(memory:GLMemory),format=RGBA`.
- Size relation: **src height = sink height / 2**, src width = sink width (sink NV12 `W×2H`, src
  RGBA `W×H`). Reject odd; assume even.

**The 2-input render (the hard part).** `render_to_target_with_shader` takes a *single* input
GLMemory, but you have two input planes (Y, UV) feeding one output. So render manually:
- In `filter()`: `in.peek_memory(0)` → Y `GLMemory` (R8 `W×2H`), `peek_memory(1)` → UV `GLMemory`
  (RG8 `W×H`); `out.peek_memory(0)` → RGBA `GLMemory` (`W×H`). Downcast each to
  `gstreamer_gl::GLMemory`; read `texture_id()`.
- Get the GL context via `GLBaseFilterExt::gl_context()`. Use it to compile a `GLShader`
  (`GLShader::new(ctx)` + default vertex stage + a custom fragment with **two samplers**
  `y_tex`, `uv_tex`) and to run raw GL: load function pointers from the context's
  `proc_address` into the `gl` crate (add `gl` as a dep — this is the standard pattern; see how
  **gst-plugins-rs** GL elements / `gstgl` examples do it), create/bind a framebuffer to the output
  texture, set viewport to `W×H`, bind `y_tex`/`uv_tex`, set `W`/`H` uniforms, draw a fullscreen
  quad. Run GL work inside the context (`ctx.thread_add(...)` / the filter's GL thread, as GstGL
  requires).
- The UV plane is `W/2 × H` RG8 (NV12 chroma); sample `.r`=U, `.g`=V at `(xc, chroma_row)`. The Y
  plane is `W×2H` R8; sample `.r`. Multiply normalized samples by 255 to use the 0..255 BT.601
  matrix (or fold 255 into the matrix). Use texel-center offsets.

**Simpler fallback if the 2-texture render stalls you (NOT zero-copy on input — last resort):**
reinterpret the decoded NV12 `W×2H` as a single `GRAY8 W×3H` buffer in sysmem (an appsink→relabel→appsrc
bridge), `glupload` that as one R8 texture, and use `MODE=Texture` +
`GLFilterExt::render_to_target_with_shader` (single input). This reintroduces a download+reupload
(defeats the zero-copy goal) but is much less GL code — use only if the proper path is blocked, and
flag it clearly.

### Shader math
Port `unpack_stacked_nv12_to_rgba` (§2 gather) into the fragment: for output `v_texcoord`→`(x,y)`
in `W×H`, branch on parity `(i,j)`, sample Y from `y_tex` and the right quadrant from `y_tex`
(aux luma tiling) or `uv_tex` (main/aux chroma), then BT.601-limited YCbCr→RGB. The CPU function is
the exact reference.

---

## 5. Integration

In [crates/viewer/src/main.rs](crates/viewer/src/main.rs):
- `make_decoder()` already branches: `if VIEWER_CHROMA.load(..) == 1 { return make_decoder_yuv444(...) }`.
- Replace the body of `make_decoder_yuv444` with the §3 pipeline (build it, insert `rmngavc444unpack`
  between `glupload` and `gtk4paintablesink`, return `(appsrc, sink.paintable)`). Drop the
  appsink/CPU-unpack bridge.
- Keep the `(AppSrc, gdk::Paintable)` return contract and the `Yuv420` path
  (`vah264dec ! glupload ! gtk4paintablesink`) unchanged.
- Also update the headless path if you touch it ([crates/viewer/src/headless.rs](crates/viewer/src/headless.rs)
  currently discards tag-4 and decodes raw → it would dump the `W×2H` stacked frame in `Yuv444`).
  Optional: add the unpack there too for `RMNG_DUMP` verification; otherwise leave a note.

---

## 6. Build, test, validate (local — the viewer runs on a normal machine)

Unlike the encoder, you can develop the viewer **locally** (it needs a display + GL, which a normal
dev box has; the existing `Yuv420` path already uses `glupload`). `cargo build -p viewer`.

**Validation ladder:**
1. **Get a `Yuv444` stream to decode.** Two options:
   - Point the viewer (`RMNG_VIDEO=<host:9001>`) at a control-server running `RMNG_CHROMA=yuv444`
     (the encoder agent / a live clone), **or**
   - Generate a stacked stream offline: synthesize a `W×H` image → `wire::avc444::pack_y444_to_stacked_nv12`
     → encode with a software/HW H.264 encoder → feed the viewer's appsrc. (See
     [crates/media/src/bin/avc444_e2e.rs](crates/media/src/bin/avc444_e2e.rs) for the pack+encode
     pattern — it produces a known stacked frame and dumps PNGs.)
2. **Pixel correctness vs the oracle**: decode one frame, run **both** your GL unpack and
   `avc444::unpack_stacked_nv12_to_rgba` on the same decoded NV12, and assert the RGBA matches
   (within a small tolerance for GPU rounding). The CPU function is ground truth.
3. **Zero-copy proof**: run with `GST_DEBUG=GST_CAPS:4` and confirm `vah264dec→glupload→rmngavc444unpack→gtk4paintablesink`
   stays on `memory:GLMemory` end-to-end (no `videoconvert`/sysmem in the path).
4. **Visual**: a 1px-colored-stripe test image must come back with the stripes intact (4:4:4), not
   blurred (4:2:0). Throughput: sustain 1440p60 with the GPU doing the reconstruction.

---

## 7. Definition of done
- In `Yuv444` mode the viewer uses the §3 all-GL pipeline; `Yuv420` decode unchanged.
- GL unpack output matches `wire::avc444::unpack_stacked_nv12_to_rgba` (oracle) within tolerance.
- Path confirmed `memory:GLMemory` end-to-end (no per-frame host copies); letterbox/cursor/fps still work (one `W×H` paintable).
- `make_decoder` returns `(AppSrc, gdk::Paintable)` as before; workspace builds; `cargo test -p wire` green (don't break the `avc444` contract).

## 8. Gotchas
- **Don't `glcolorconvert` the NV12 to RGBA before your element** — that 4:2:0-upsamples the chroma and **destroys the packed data**. Your element must read the **raw** Y/UV GL textures.
- **`MODE=Buffer` + manual 2-texture FBO render** is required for true zero-copy (the single-input `render_to_target_with_shader` helper can't take both planes). Budget time for the GL plumbing (proc-address loading via the `gl` crate, FBO, GL-thread affinity).
- GstGL work must run on the element's GL thread/context (use the context's thread-add mechanism), not an arbitrary thread.
- **Don't change `crates/wire/src/avc444.rs`** — it's the contract with the encoder agent.
- The viewer's GL context comes from `gtk4paintablesink`/GTK; your element shares it via the standard GstGL context propagation — don't create your own display.
