# Porting `rmng-viewer` to macOS (Apple Silicon)

Implementation + testing handoff. Audited 2026-07-02 against GStreamer/GTK/gst-plugins-rs
sources and release notes (all load-bearing claims below were verified in primary sources, not
recalled). **Scope: the viewer only** (`crates/viewer` + small `crates/wire` fixes). The encode
side stays on Linux; the Mac only decodes/displays and sends input.

**Verdict:** the zero-copy architecture survives on macOS intact. VideoToolbox decodes into
IOSurface-backed `GstGLMemory`, `gtk4paintablesink` shares GTK's CGL context the same way it
shares EGL on Linux, and the frame never touches the CPU. The two mandatory video-path changes
are (a) rectangle-texture support in `rmngavc444unpack` and (b) a GLSL dialect port. Most of the
genuinely new code is in the input layer (keymap, pointer lock).

---

## 1. What the viewer is (context for the implementing agent)

Native GTK4 remote-desktop viewer. Per remote monitor it runs one GStreamer decode pipeline fed
by a TCP stream of H.264 access units, and forwards input/clipboard back over the same socket.

Transport (portable, do not change — the Linux server side speaks exactly this):

- One TCP connection ("port 1", default `127.0.0.1:9001`, env `RMNG_VIDEO`, config in
  [crates/viewer/src/config.rs](crates/viewer/src/config.rs)).
- Control messages: `[u8 tag][u32be len][json]`, tags: 0=input, 1=clipboard, 2=cursor shape,
  3=layout, 4=mode. Tag 4 (`ModeMsg`) arrives before the first AU and selects chroma mode:
  `Yuv420` (plain H.264) or `Yuv444` (AVC444: a double-height `W×2H` NV12 stream carrying main
  luma/chroma + packed auxiliary chroma, reconstructed to 4:4:4 RGBA on the GPU).
- Video AUs: `[u32be monitor_id][u32be len][AnnexB]`.
- **Input messages carry Linux evdev codes on the wire** (the server injects them via uinput).
  The viewer must translate whatever the local platform gives it into evdev.

Current Linux pipelines (`gst::parse::launch` strings):

- 4:2:0 — [crates/viewer/src/main.rs:770-771](crates/viewer/src/main.rs#L770-L771):
  `appsrc name=src is-live=true format=time do-timestamp=true ! h264parse ! vah264dec ! glupload ! gtk4paintablesink name=sink sync=false`
- 4:4:4 — [crates/viewer/src/main.rs:823-824](crates/viewer/src/main.rs#L823-L824):
  same but `… glupload ! rmngavc444unpack ! gtk4paintablesink …`
- Headless (`--headless`, CI/testing, no GTK) — [crates/viewer/src/headless.rs:30-44](crates/viewer/src/headless.rs#L30-L44):
  four variants of the same, ending in `appsink` (plus `gldownload ! videoconvert ! pngenc` when
  `RMNG_DUMP=*.png` is set).
- appsrc caps everywhere: `video/x-h264, stream-format=byte-stream, alignment=au`.
  `sync=false` on the sink is deliberate (render-on-arrival, latest-wins live paintable, no
  audio) — keep it.

`rmngavc444unpack` ([crates/viewer/src/glunpack.rs](crates/viewer/src/glunpack.rs)) is a
`GstGLFilter` subclass: sink = NV12 `W×2H` GLMemory (2 planes as GL textures: Y=R8, UV=RG8),
src = RGBA `W×H` GLMemory. One FBO render (attributeless `gl_VertexID` fullscreen triangle) does
the polyphase chroma gather + BT.601-limited YCbCr→RGB. Its CPU oracle is
`wire::avc444::unpack_stacked_nv12_to_rgba`; `rmng-viewer --glunpack-validate [W H]` compares
GPU vs oracle (passes at max abs err 1 on Linux). Two hard-won details already in the code that
**must be preserved** (documented inline in glunpack.rs):

1. The output buffer is mapped `WRITE|GL` after the FBO render (without it, downstream reads
   stale CPU bytes).
2. A VAO is bound around the draw when `glGenVertexArrays` exists (mandatory on desktop-GL core
   contexts — which is exactly what macOS CGL gives you).

---

## 2. Verified macOS ecosystem facts (July 2026)

| # | Fact | Source |
|---|------|--------|
| 1 | `vtdec`/`vtdec_hw` (applemedia plugin) output **NV12 `GstGLMemory` zero-copy** on macOS via IOSurface: the decoder's CVPixelBuffer IOSurface is bound with `CGLTexImageIOSurface2D`, per plane (Y, UV as two textures — same 2-plane structure as Linux `glupload`). | `sys/applemedia/vtdec.c`, `iosurfaceglmemory.c`, `videotexturecache-gl.m` (GStreamer main / 1.28) |
| 2 | On macOS those GL textures are **`texture-target=rectangle`, always** (Apple requirement: `CGLTexImageIOSurface2D` only accepts `GL_TEXTURE_RECTANGLE`; iOS gets 2D). vtdec's GLMemory caps on 1.28.3+/main are NV12-only. | `vtdec.c` `gst_vtdec_negotiate()` `#if TARGET_OS_OSX`; SDK header `CGLIOSurface.h` ("Must currently be GL_TEXTURE_RECTANGLE_ARB") |
| 3 | `glupload` has **no IOSurface/CVPixelBuffer path** (its non-Linux methods are GLMemory passthrough / raw CPU upload). vtdec emits GLMemory itself, so `glupload` simply drops out of the macOS pipelines. | `gstglupload.c` (zero `__APPLE__`/IOSurface refs) |
| 4 | `gtk4paintablesink` supports **GL zero-copy on macOS by default** (no cargo feature, unlike Linux) since gst-plugin-gtk4 0.10.0 (Feb 2023). Mechanism: makes GTK's `GdkMacosGLContext` current, wraps the CGL handle via `gst_gl_context_new_wrapped(GST_GL_PLATFORM_CGL)`, answers the GstGL context query so upstream GL elements share the context. | `video/gtk4/src/sink/imp.rs` `initialize_macosgl()`; README; CHANGELOG 0.10.0 |
| 5 | The sink's GL caps accept **RGBA/RGB, `texture-target=2D` only** (`GdkGLTextureBuilder` has no texture-target property). Non-sharing/non-GL input silently falls back to a sysmem copy — a working-but-slow trap to watch for. | `imp.rs` (`GL_FORMATS`, `texture-target` set to `2D`), `frame.rs`; docs.gtk.org GLTextureBuilder |
| 6 | **GStreamer 1.28 (2026-01-27) official macOS universal `.pkg` ships GTK4 + gtk4paintablesink** (first release to do so). Homebrew `gstreamer` 1.28.4 also builds the gtk4 plugin (`-Dgst-plugins-rs:gtk4=enabled`) against brew's `gtk4`. | 1.28 release notes ("GTK4 is now shipped on macOS and Windows"); GNOME Discourse announcement 2026-01-10; Homebrew formula |
| 7 | **Apple's OpenGL (4.1 core max, GL-on-Metal) has no `GL_ARB_ES3_compatibility`** on any Mac incl. M1→M4 → `#version 300 es` shaders are **rejected**. Desktop GLSL `140/150/330/400/410` accepted; ≤130 rejected. `gl_VertexID`, `in/out`, `texture()` all fine in 330/410 core; `precision` qualifiers are legal no-ops in desktop GLSL. GstGL's GLSL auto-mangling only applies to its *internal* shaders, not custom GLFilters. | opengl.gpuinfo.org reports 10696 (M1) / 14966 (M4) full extension lists; ARB_ES3_compatibility spec; GLSL 4.10 spec §3.3 |
| 8 | GstGL on macOS = platform **CGL**, window system Cocoa, desktop GL only (requests 3.2-core "or later" → reports 4.1 on Apple Silicon; GLES rejected). `gst_gl_context_new_wrapped` with a CGL handle is a shipped, supported configuration (Qt qmlglsink does the same). | `gstglcontext_cocoa.m` |
| 9 | `glcolorconvert` **can** convert texture-target rectangle→2D and NV12→RGBA in one GPU pass (pad templates list `{2D, rectangle, external-oes}`; shader mangling swaps `sampler2DRect` and un-normalizes coords). | `gstglcolorconvert.c` |
| 10 | **GTK's legacy `gl` GSK renderer was removed in GTK 4.18.** macOS default renderer is `ngl` (GL 3.3+ over CGL). No Metal renderer exists; Vulkan-via-MoltenVK is experimental (4.19+). Homebrew ships GTK 4.22. | GTK NEWS; gdkmacosglcontext.c |
| 11 | Pointer lock: GTK4 has **no API** (deliberate). The canonical native pattern (verified in SDL/GLFW/winit source): `CGAssociateMouseAndMouseCursorPosition(false)` + hide cursor + `CGWarpMouseCursorPosition`, deltas from `NSEvent deltaX/deltaY`. An **NSEvent local monitor needs no TCC permission**; a `CGEventTap` triggers the Input-Monitoring prompt — avoid. macOS 14+ `GCMouse` gives raw *unaccelerated* deltas. Rust: `core-graphics` (CG functions) + `objc2-app-kit` (`NSEvent`, monitors need the `block2` feature); `gdk4-macos` crate exposes the NSWindow if needed. | SDL `SDL_cocoamouse.m`; GLFW `cocoa_window.m`; winit appkit backend; Apple docs; docs.rs |
| 12 | GDK on macOS reports `hardware_keycode` as the **Carbon virtual keycode (`kVK_*`, 0–127)** — the viewer's `evdev = hardware_keycode − 8` X11/Wayland convention does not hold. | GDK macOS backend keymap |
| 13 | GTK-macOS backend is maintained but second-tier: known IME gaps (GTK#3968), a "duplicate window on activate" quirk seen with gtk4paintablesink examples (GTK#7579), HiDPI via `backingScaleFactor` (`GDK_SCALE` ignored). No upstream macOS CI for gtk4paintablesink. | GTK/GStreamer trackers, Discourse |

---

## 3. Dev environment setup (on the Mac)

```sh
brew install gtk4 gstreamer pkgconf
rustup target list | grep aarch64-apple-darwin   # native toolchain; no cross-compile from Linux
```

Homebrew's `gstreamer` formula is the monorepo build: base/good/bad (incl. applemedia) +
gst-plugins-rs incl. `gtk4paintablesink`, linked against **brew's** `gtk4`. Since the viewer
also links GTK4 via pkg-config, using brew for both keeps a single GTK in the process.

> **Two-GTK hazard:** the official GStreamer.framework `.pkg` bundles its *own* GTK4 for the
> gtk4 plugin. Do not mix framework GStreamer with brew GTK (or vice versa) in one process —
> pick one provider for both. For development use brew for everything; revisit for packaging.

Sanity checks before touching code:

```sh
gst-inspect-1.0 vtdec_hw           # applemedia HW-only decoder (vtdec = HW with SW fallback)
gst-inspect-1.0 gtk4paintablesink
gst-inspect-1.0 glcolorconvert
# End-to-end smoke (sink spawns its own window when run standalone):
gst-launch-1.0 videotestsrc ! vtenc_h264 ! h264parse ! vtdec_hw ! glcolorconvert ! gtk4paintablesink
```

Rust bindings in the workspace (gstreamer 0.23 w/ `v1_24`, gtk4 0.9 w/ `v4_14`) are compatible
with runtime GStreamer 1.28 / GTK 4.22 — no bumps required.

---

## 4. Work items

Ordered so the crate compiles first, then the video path lights up, then input.

### 4.0 Make it compile on macOS (blockers are in Cargo.toml, not code)

- [crates/viewer/Cargo.toml](crates/viewer/Cargo.toml): `gdk4-wayland` links the C
  `gtk4-wayland` library — **will not build on macOS**. Move `gdk4-wayland`, `wayland-client`,
  `wayland-protocols` under `[target.'cfg(target_os = "linux")'.dependencies]`.
- Gate the Wayland pointer-lock module
  ([crates/viewer/src/pointer_lock.rs](crates/viewer/src/pointer_lock.rs)) with
  `#[cfg(target_os = "linux")]` (it already degrades gracefully at *runtime* on non-Wayland via
  downcast failure — the problem is purely compile/link). Keep its public surface
  (`PointerLock::new() -> Option<…>`, `engage`, `disengage`, `is_engaged`) so `main.rs` call
  sites stay identical; add a macOS twin later (4.5).
- [crates/wire/src/net.rs:34-44](crates/wire/src/net.rs#L34-L44) `set_keepalive`:
  `socket2::Socket::set_tcp_user_timeout` only exists on Linux targets → cfg-gate that one call.
  `TcpKeepalive::with_time/with_interval/with_retries` work on macOS — keep them.
- The GTK-4.20-Wayland stuck-cursor workaround in `install_pointer`'s `connect_enter`
  (named-cursor bounce, see [crates/viewer/src/main.rs](crates/viewer/src/main.rs), search
  `set_cursor_from_name`) is Wayland-bug-specific; it's harmless elsewhere but gate it
  `#[cfg(target_os = "linux")]` for clarity.
- `GSK_RENDERER=gl` pin at [crates/viewer/src/main.rs:64-66](crates/viewer/src/main.rs#L64-L66):
  the legacy `gl` renderer no longer exists in GTK ≥ 4.18, so on macOS this would force a
  fallback warning. Make the pin Linux-only. (See risk §6.2 — the bug it works around may need a
  different mitigation on macOS.)
- Config paths ([crates/viewer/src/config.rs](crates/viewer/src/config.rs)) use
  `$XDG_CONFIG_HOME`/`$HOME/.config` — works on macOS as-is; optionally switch to the `dirs`
  crate later. Not a blocker.

**Checkpoint:** `cargo build -p viewer` succeeds on the Mac.

### 4.1 Decoder swap (4:2:0 path first)

macOS pipeline (in `make_decoder`, [crates/viewer/src/main.rs:762](crates/viewer/src/main.rs#L762)):

```
appsrc name=src is-live=true format=time do-timestamp=true ! \
  h264parse ! vtdec_hw ! glcolorconvert ! gtk4paintablesink name=sink sync=false
```

- `vtdec_hw` replaces `vah264dec`; `glupload` drops out (fact §2.1/§2.3). Prefer `vtdec_hw`
  (fails rather than silently software-decoding); fall back to `vtdec` only deliberately.
- `glcolorconvert` is **required** on macOS even though Linux has none: the sink only takes
  RGBA/RGB 2D GLMemory (fact §2.5), and vtdec emits NV12 rectangle. On Linux the equivalent
  conversion happened implicitly inside `glupload`'s EGL dmabuf import — do not "simplify" the
  Linux pipeline to match.
- `h264parse` negotiates byte-stream→AVC for VideoToolbox automatically; appsrc caps unchanged.
- Select the pipeline string per-OS (`#[cfg]` or runtime `gst::ElementFactory::find`), keeping
  the Linux strings byte-identical to today's.

**Checkpoint:** live 4:2:0 against the staging server (§5.3) shows the desktop, and
`GST_DEBUG=gtk4paintablesink:5` log confirms the GL path (no sysmem-upload fallback messages;
also see §5.6 zero-copy verification).

### 4.2 `rmngavc444unpack`: rectangle-texture support

All in [crates/viewer/src/glunpack.rs](crates/viewer/src/glunpack.rs). Goal: sink pad accepts
`texture-target` ∈ {`2D`, `rectangle`}; src pad stays `RGBA`/`2D` (which is both what the sink
demands and what GLFilter's output pool allocates). Linux behavior must not change (glupload
yields 2D there; the 2D path is also what `--glunpack-validate` exercises on macOS, since raw
sysmem `glupload` produces 2D textures).

- **Pad templates** ([glunpack.rs:183-211](crates/viewer/src/glunpack.rs#L183-L211)): sink caps
  `texture-target` becomes a list `{2D, rectangle}` (use `gst::List`); src stays `"2D"`.
- **`transform_internal_caps`** ([glunpack.rs:237-264](crates/viewer/src/glunpack.rs#L237-L264)):
  currently forces `texture-target=2D` both directions. Direction sink→src: set `"2D"`.
  Direction src→sink: offer the list `{2D, rectangle}`.
- **`set_caps`**: `VideoInfo` does not carry texture-target — parse it from the caps structure
  (`incaps.structure(0).get::<&str>("texture-target")`) and store it in `State`.
- **Shader build** (`build_unpack_shader`,
  [glunpack.rs:384-392](crates/viewer/src/glunpack.rs#L384-L392)): two axes now —
  1. *GLSL dialect*: `GLSLProfile::ES` + `#version 300 es` is hardcoded; Apple rejects it (fact
     §2.7). Pick per context: if `ctx.gl_api()` contains `OPENGL3` → desktop `#version 330 core`
     (or `410 core`), `GLSLVersion`/`GLSLProfile::Core` accordingly; GLES3 contexts keep
     `300 es`. Port of the source text is mechanical: swap the version line, drop the two
     `precision` statements (or keep — legal no-ops in core), everything else (`in`/`out`,
     `texture()`, `gl_VertexID`) is dialect-identical. **Do not rely on GstGL to mangle it**
     (fact §2.7). Mesa accepts `330 core`, so you may unify Linux+macOS on desktop GLSL when the
     context is desktop GL — Linux revalidation required (§5.2).
  2. *Sampler type*: negotiated `rectangle` input → declare `y_tex`/`uv_tex` as `sampler2DRect`
     and sample with **unnormalized** texel coords — the `lum()`/`chr()` helpers
     ([glunpack.rs:82-88](crates/viewer/src/glunpack.rs#L82-L88)) already compute texel
     coordinates first, so the rect variant just drops the divide:
     `texture(y_tex, vec2(sx + 0.5, sy + 0.5))`. Keep the `w`/`h` uniforms (the gather
     arithmetic still needs them). Generate the two shader variants from one template (string
     substitution is fine); compile lazily per negotiated caps as today.
- **`unpack_render`** ([glunpack.rs:396+](crates/viewer/src/glunpack.rs#L396)): bind input
  textures to the negotiated target — `GL_TEXTURE_RECTANGLE = 0x84F5` vs `GL_TEXTURE_2D =
  0x0DE1` — including the `glTexParameteri` NEAREST/clamp calls. The FBO color attachment
  (output tex) stays `GL_TEXTURE_2D`. The VAO and the `WRITE|GL` output map (§1) stay exactly
  as they are — macOS CGL is the desktop-core case they exist for.
- macOS 4:4:4 pipeline (in `make_decoder_yuv444`,
  [crates/viewer/src/main.rs:817](crates/viewer/src/main.rs#L817)):

  ```
  appsrc … ! h264parse ! vtdec_hw ! rmngavc444unpack ! gtk4paintablesink name=sink sync=false
  ```

  **Never insert `glcolorconvert` before the unpacker** — it would 4:2:0-convert and destroy the
  packed chroma (existing warning at
  [main.rs:814-816](crates/viewer/src/main.rs#L814-L816) applies doubly here; the whole point of
  rect support in the filter is to eat vtdec's output directly).
- Headless ([crates/viewer/src/headless.rs:32-44](crates/viewer/src/headless.rs#L32-L44)): same
  substitutions (`vah264dec ! glupload` → `vtdec_hw`); the dump path's trailing
  `gldownload ! videoconvert ! pngenc` is portable. Note `gldownload` from the unpacker's RGBA
  2D output is fine on macOS (the AMD dmabuf-export limitation from the encode side is
  irrelevant here — this is a plain glReadPixels-style download).

**Checkpoint:** `--glunpack-validate` passes on the Mac (2D path), and headless `RMNG_DUMP`
against the staging server in Yuv444 reconstructs a pixel-perfect PNG (rectangle path).

### 4.3 Keyboard: kVK → evdev table

[crates/viewer/src/main.rs:1141-1143](crates/viewer/src/main.rs#L1141-L1143) does
`keycode = hardware_keycode − 8` (X11/Wayland evdev offset). On macOS, `hardware_keycode` is the
Carbon `kVK_*` code (fact §2.12) — the wire needs evdev (`input-event-codes.h`) codes, so add a
`#[cfg(target_os = "macos")]` translation table (~100 entries, kVK 0–127 → evdev). Derivation
sources: Chromium's `dom_code_data.inc` (columns for USB HID ↔ kVK ↔ evdev), or any
Moonlight/Parsec-style client. Verify empirically first (log `hardware_keycode` for a few keys)
— don't trust the table blind; GDK backends have surprised before.

Mouse buttons need **no change**: `evdev_button()`
([crates/viewer/src/main.rs:1010-1038](crates/viewer/src/main.rs#L1010-L1038) area) maps GTK
button numbers (identical on macOS) to wire-protocol constants.

Scroll ([main.rs:1040-1053](crates/viewer/src/main.rs#L1040-L1053)): discrete ±1 per event via
`EventControllerScroll`. Test with a trackpad — GTK-macOS may deliver fine-grained/momentum
events; if scrolling feels wrong, accumulate smooth deltas into discrete steps.

Keyboard-shortcut inhibition
([main.rs:1066-1085](crates/viewer/src/main.rs#L1066-L1085)) uses the Wayland
`inhibit_system_shortcuts` protocol — silent no-op on macOS; leave it. Consequence: Cmd-Tab /
Cmd-Space go to the host. Capturing them requires a permission-gated CGEventTap (what commercial
remote-desktop apps do) — out of scope for the first pass; note it in the UI/README instead.

### 4.4 Pointer lock (macOS twin of `pointer_lock.rs`)

New `#[cfg(target_os = "macos")]` module with the same API. Recipe (verified as what
SDL/GLFW/winit ship, fact §2.11):

- engage: `CGAssociateMouseAndMouseCursorPosition(false)` + `NSCursor::hide()` (or
  `CGDisplayHideCursor`); optionally `CGWarpMouseCursorPosition` to the window center once.
  While disassociated the cursor stays frozen, so no per-frame warping is needed.
- deltas: `NSEvent.addLocalMonitorForEventsMatchingMask(MouseMoved|LeftMouseDragged|…)`, read
  `deltaX()/deltaY()`, feed the same relative-motion path the Wayland module feeds today
  (`Dispatch<ZwpRelativePointerV1>` equivalent). Local monitors run in-process on the main
  thread and require **no TCC permission**. Do **not** use `CGEventTap` (Input-Monitoring
  prompt).
- disengage: re-associate + unhide + remove the monitor.
- Crates: `core-graphics` (0.25: `CGAssociateMouseAndMouseCursorPosition`,
  `CGDisplayHideCursor`, `CGWarpMouseCursorPosition` in `core_graphics::display`) and
  `objc2-app-kit` (`block2` feature for the monitor closure). Add under
  `[target.'cfg(target_os = "macos")'.dependencies]`.
- Caveat: `NSEvent.deltaX/Y` are *accelerated*. If in-game aiming feels off, the upgrade is
  `GCMouse.mouseMovedHandler` (GameController framework, macOS 14+) for raw deltas — SDL3 does
  exactly this. Ship accelerated first, note the follow-up.

The remote cursor texture path (`gdk::Cursor::from_texture`,
[main.rs:495-502](crates/viewer/src/main.rs#L495-L502) area) is portable GTK — no change
expected; verify visually.

### 4.5 Nice-to-have (not first pass)

`~/Library/Application Support` config dir; Cmd↔Ctrl remap option for the remote Linux session;
.app bundling (§7).

---

## 5. Test plan (run in this order)

1. **Element smoke** (§3 commands) — proves brew stack before any code runs.
2. **`cargo run -p rmng-viewer -- --glunpack-validate 256 144`** then `2560 1440`. Pass = max
   abs RGB err ≤ 1 vs the CPU oracle (same bar as Linux). Exercises the desktop-GLSL port +
   2D path. `RMNG_GL_DEBUG=1` logs FBO status/GL errors on trouble. **Also rerun on Linux** if
   you changed the shader dialect selection — the Intel dev box must stay at err ≤ 1.
3. **Headless vs staging server**: the deployed control-server on the LAN is
   `RMNG_VIDEO=10.0.0.79:9001` (chroma mode is a *server-side* setting — chroma set to 4:4:4 in
   Settings → Video, restart-required; the viewer learns it from the tag-4 handshake). Run
   `RMNG_VIDEO=10.0.0.79:9001 RMNG_DUMP=frame.png rmng-viewer --headless` in both server modes.
   Yuv444 pass = crisp colored text (chroma preserved), correct colors/orientation.
   The headless log prints the first decoded frame's caps
   ([headless.rs:59](crates/viewer/src/headless.rs#L59)) — on macOS expect
   `memory:GLMemory, format=NV12, texture-target=rectangle` out of vtdec.
4. **Live GUI, 4:2:0 then 4:4:4**: desktop renders, ~60 fps under motion (kgx text-flood on the
   clone generates continuous full-rate damage), cursor shape updates, clipboard both ways,
   window resize/fullscreen/letterboxing.
5. **Input correctness**: every key reaches the remote correctly (type the full keyboard into a
   remote editor); buttons; trackpad + wheel scroll; pointer lock in something Minecraft-like
   (engage, look around, disengage; check for drift or acceleration weirdness).
6. **Zero-copy verification** (catch the silent sysmem fallback, fact §2.5): CPU% of the viewer
   in Activity Monitor while streaming 1440p60 should be low single-digit-to-teens; a CPU-copy
   path shows up immediately at `W×2H` sizes. Corroborate with
   `GST_DEBUG=gtk4paintablesink:6,glcolorconvert:5` (look for "share"/"wrapped context" success,
   absence of repeated upload messages).
7. **Latency feel + VT reordering check** (risk §6.1): wave the mouse and compare local vs
   remote cursor lag vs the Linux viewer. If the Mac consistently trails by fixed frames,
   suspect VideoToolbox DPB buffering.
8. **Stale-frame regression** (risk §6.2): non-maximized, *downscaled* viewer window; drag
   remote windows around; watch for an **old frame from several frames back** flashing (not
   tearing). Clean at 1:1/maximized + dirty when small = the GSK texture-caching bug.

---

## 6. Known risks & mitigations

1. **VideoToolbox reorder latency** (medium). Unlike VA-API, VT may hold frames for DPB
   reordering unless the stream's SPS/VUI declares zero reorder (`max_num_reorder_frames=0` /
   pic-order constraints). `sync=false` makes the *sink* immune to declared latency, but not to
   frames the *decoder* physically retains. If test §5.7 shows fixed lag, the fix is on the
   **Linux encoder side** (vah264enc SPS flags) — report it back rather than hacking the viewer.
2. **GSK `ngl` stale-texture bug with no `gl` escape hatch** (medium). Linux pins
   `GSK_RENDERER=gl` because ngl/vulkan cache recycled `GdkTexture` objects by identity
   ([main.rs:56-66](crates/viewer/src/main.rs#L56-L66)); the legacy renderer is gone in GTK
   ≥4.18, and macOS defaults to ngl, where the sink still recycles texture ids. If §5.8
   reproduces: the designed fallback is rendering the latest frame ourselves via a `GtkGLArea`
   fed by `appsink max-buffers=1 drop=true` (glimagesink's latest-wins behavior, in-app) instead
   of `for_paintable`. Budget for this; it's the most likely surprise.
3. **GTK-macOS paper cuts** (low): duplicate-window-on-activate quirk (GTK#7579 — if two windows
   appear, it's that, not your pipeline), IME gaps, fullscreen/window-management oddities.
   Fine for a single-window viewer; expect polish, not showstoppers.
4. **Apple GL deprecation** (strategic, not current): GL 4.1-on-Metal is deprecated-but-shipping
   and is what GTK itself uses on macOS. No GTK Metal renderer exists. The GL path is the
   supported path today; just don't build *new* macOS-only features on exotic GL.
5. **No upstream macOS CI for gtk4paintablesink** — behavior verified by maintainers + users,
   not CI. Pin the working brew versions once green (`brew list --versions gtk4 gstreamer`) and
   record them here.

---

## 7. Packaging (after it works)

- Dev/dogfood: plain brew deps + the binary.
- Distributable `.app`: either (a) official GStreamer 1.28 framework
  (`/Library/Frameworks/GStreamer.framework` — includes GStreamer + GTK4 + gtk4paintablesink;
  build the viewer against the framework's pkg-config so its GTK is the *only* GTK), or
  (b) all-Homebrew + `gtk-mac-bundler`/`install_name_tool` relocation. Never mix GTK providers
  (§3 hazard). No Linux→macOS cross-compile exists; CI = GitHub Actions arm64 macOS runner.
- No flagship GTK4+GStreamer app ships an official macOS bundle yet — treat bundling as
  pioneering; budget a day or two of dylib-path debugging.

---

## 8. Invariants — do not change

- Wire protocol (framing, tags, evdev codes on the wire, AVC444 packing) — the Linux server and
  the `wire::avc444` CPU oracle define correctness.
- `sync=false` + `appsrc do-timestamp=true` semantics on all pipelines.
- No `glcolorconvert`/`videoconvert` between decoder and `rmngavc444unpack` in Yuv444.
- The `WRITE|GL` output map and VAO bind in `glunpack.rs`.
- Linux pipelines and Linux shader behavior (`--glunpack-validate` must stay err ≤ 1 on
  Intel/Mesa after any shader-selection refactor).
