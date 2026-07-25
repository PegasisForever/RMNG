//! `rmngavc444unpack` — one custom GstGL filter that reconstructs full-resolution `W×H` RGBA
//! from a decoded **AVC444 stacked `W×2H` NV12** frame, entirely on the GPU (zero host copies).
//!
//! This is the decode-side mirror of [`media::glpack`]'s `rmngavc444pack`: the encoder packs the
//! image's luma + base chroma into the top half of a `W×2H` NV12 frame and the dropped chroma
//! into the bottom half (see [`wire::avc444`]); the viewer H.264-decodes that to `W×2H` NV12 and
//! this element gathers the polyphase chroma quadrants back into `W×H` 4:4:4 + does the
//! BT.601-limited YCbCr→RGB, all in a single FBO render. It replaces the old CPU bridge
//! (`vah264dec ! videoconvert ! NV12 ! appsink → unpack_stacked_nv12_to_rgba → appsrc`), which
//! cost ~26 MB of GPU↔CPU transfer + a full-frame matrix pass per frame.
//!
//! Pipeline position (see [`crate::make_decoder_yuv444`]):
//! `vah264dec ! glupload ! rmngavc444unpack ! gtk4paintablesink` — `memory:GLMemory` end to end.
//!
//! - **sink**: NV12 `W×2H` GLMemory (2 textures: Y as R8, UV as RG8). On Linux `glupload` yields
//!   `texture-target=2D`; on macOS `vtdec_hw` emits IOSurface-backed `texture-target=rectangle`
//!   (Apple's `CGLTexImageIOSurface2D` only accepts `GL_TEXTURE_RECTANGLE`). Both are accepted.
//! - **src**: RGBA `W×H` GLMemory (the reconstructed image; `gtk4paintablesink` demands 2D and
//!   displays it zero-copy, intrinsic size `W×H` so the viewer's letterbox/cursor/fps logic is
//!   unchanged).
//!
//! Built on [`gst_gl::GLFilter`] (`MODE = GLFilterMode::Buffer`, so we see both NV12 planes):
//! GLFilter calls our [`filter`](imp::Avc444Unpack) **already on the element's GL thread with the
//! context current** (via `gst_gl_context_thread_add` inside `gst_gl_filter_transform`), waits the
//! input GL-sync-meta before, and sets the output sync point after — so we just bind the two input
//! textures, FBO-render the unpack shader into the output texture, and `glFlush`. The GL context
//! is shared from `gtk4paintablesink`/GTK by the standard GstGL context propagation.
//!
//! ## Shader dialect selection
//!
//! Apple's OpenGL (4.1 core / GL-on-Metal) has no `GL_ARB_ES3_compatibility`, so `#version 300 es`
//! shaders are **rejected**. We detect the context API at runtime:
//! - `GLES2` context → `#version 300 es` + `precision` qualifiers (Linux, original behavior).
//! - `OPENGL3` context → `#version 330 core`, no precision qualifiers (macOS CGL / Mesa desktop).
//!
//! GstGL's GLSL auto-mangling applies only to its *internal* shaders; for custom GLFilters we must
//! embed the version line in the source ourselves. We do, and tell GstGL the matching metadata.
//!
//! ## Rectangle-texture support
//!
//! `vtdec_hw` on macOS emits `texture-target=rectangle` (GL_TEXTURE_RECTANGLE, unnormalized
//! coords). The shader switches sampler type and coordinate expressions accordingly:
//! - 2D: `sampler2D`, normalized coords `(sx+0.5)/w / (sy+0.5)/(2h)`.
//! - rect: `sampler2DRect`, unnormalized coords `sx+0.5 / sy+0.5` (the texel-center offsets
//!   already computed by the lum()/chr() helpers — the rect variant just drops the divides).
//! The FBO output attachment always stays `GL_TEXTURE_2D` (gtk4paintablesink demands 2D).

use std::sync::{Mutex, OnceLock};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer::subclass::prelude::*;
use gstreamer_base as gst_base;
use gstreamer_base::subclass::BaseTransformMode;
use gstreamer_base::subclass::prelude::*;
use gstreamer_gl as gst_gl;
use gstreamer_gl::prelude::*;
use gstreamer_gl::subclass::GLFilterMode;
use gstreamer_gl::subclass::prelude::*;
use gstreamer_gl::{GLSLProfile, GLSLStage, GLSLVersion, GLShader, GLVideoFrameRef};
use gstreamer_video::VideoInfo;

/// `GL_FRAGMENT_SHADER` / `GL_VERTEX_SHADER` GLenums (no Rust constants in the crate).
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_VERTEX_SHADER: u32 = 0x8B31;

/// Vertex shader body (version line is prepended at runtime based on the GL context API).
/// Emits `v_texcoord` in `[0,1]` matching the viewport via the VBO-less `gl_VertexID` trick.
/// Identical convention to `rmngavc444pack` (validated byte-identical to the CPU oracle).
const VERT_BODY: &str = r#"out vec2 v_texcoord;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    v_texcoord = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// Fragment shader body template: placeholders are filled by [`build_unpack_shader`].
/// - `{SAMPLER}` → `sampler2D` (2D textures, normalized coords) or `sampler2DRect` (rectangle).
/// - `{LUM}` → texel-coord expression for the luma plane sample.
/// - `{CHR}` → texel-coord expression for the chroma plane sample.
///
/// The gather is the exact inverse of [`wire::avc444::unpack_stacked_nv12_to_rgba`] (CPU oracle).
/// For each output pixel `(x,y)` in `W×H`, with `i=x&1`, `j=y&1`, `xc=x>>1`, `yc=y>>1`:
/// - `Y` = luma `(x, y)` (main luma, top region of the `W×2H` plane)
/// - `Cb`: (0,0)→chroma U(xc,yc) · (1,0)→luma(xc,H+yc) · (0,1)→luma(cw+xc,H+yc) · (1,1)→luma(xc,H+ch+yc)
/// - `Cr`: (0,0)→chroma V(xc,yc) · (1,0)→luma(cw+xc,H+ch+yc) · (0,1)→chroma U(xc,ch+yc) · (1,1)→chroma V(xc,ch+yc)
///
/// `y_tex` is the R8 `W×2H` luma plane; `uv_tex` the RG8 `W/2×H` NV12 chroma plane.
/// NEAREST sampling + texel-center offsets; results scaled ×255 for the 0..255 matrix then
/// divided back to [0,1] for the UNORM8 store (matching the CPU oracle's round().clamp()).
const FRAG_BODY: &str = r#"in vec2 v_texcoord;
uniform {SAMPLER} y_tex;    // R8,  W   x 2H  (luma + aux-luma tiling)
uniform {SAMPLER} uv_tex;   // RG8, W/2 x H   (NV12 chroma: main over aux)
uniform float w;            // output image width  W
uniform float h;            // output image height H
out vec4 frag;

// luma texel (sx,sy) in the W x 2H plane → 0..255
float lum(float sx, float sy) {
    return texture(y_tex, {LUM}).r * 255.0;
}
// chroma texel (cx,cy) in the (W/2) x H plane → (U,V) 0..255
vec2 chr(float cx, float cy) {
    return texture(uv_tex, {CHR}).rg * 255.0;
}

void main() {
    float x  = floor(v_texcoord.x * w);
    float y  = floor(v_texcoord.y * h);
    float i  = mod(x, 2.0);
    float j  = mod(y, 2.0);
    float xc = floor(x * 0.5);
    float yc = floor(y * 0.5);
    float cw = floor(w * 0.5);   // W/2
    float ch = floor(h * 0.5);   // H/2

    float Y = lum(x, y);

    float Cb;
    if (i < 0.5) {
        if (j < 0.5) Cb = chr(xc, yc).x;            // (0,0) main U  = Cb00
        else         Cb = lum(cw + xc, h + yc);     // (0,1) aux TR  = Cb10
    } else {
        if (j < 0.5) Cb = lum(xc, h + yc);          // (1,0) aux TL  = Cb01
        else         Cb = lum(xc, h + ch + yc);     // (1,1) aux BL  = Cb11
    }

    float Cr;
    if (i < 0.5) {
        if (j < 0.5) Cr = chr(xc, yc).y;            // (0,0) main V  = Cr00
        else         Cr = chr(xc, ch + yc).x;       // (0,1) aux U   = Cr10
    } else {
        if (j < 0.5) Cr = lum(cw + xc, h + ch + yc);// (1,0) aux BR  = Cr01
        else         Cr = chr(xc, ch + yc).y;       // (1,1) aux V   = Cr11
    }

    float c = (Y - 16.0) * 1.164383;
    float d = Cb - 128.0;
    float e = Cr - 128.0;
    vec3 rgb = vec3(c + 1.596027 * e,
                    c - 0.391762 * d - 0.812968 * e,
                    c + 2.017232 * d);
    frag = vec4(clamp(rgb / 255.0, 0.0, 1.0), 1.0);
}
"#;

fn cat() -> &'static gst::DebugCategory {
    static CAT: OnceLock<gst::DebugCategory> = OnceLock::new();
    CAT.get_or_init(|| {
        gst::DebugCategory::new(
            "rmngavc444unpack",
            gst::DebugColorFlags::empty(),
            Some("RMNG AVC444 zero-copy GL unpacker"),
        )
    })
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Avc444Unpack {
        state: Mutex<State>,
    }

    struct State {
        /// Negotiated sink (NV12 `W×2H` GLMemory) video info, for the input GL frame map + uniforms.
        in_info: Option<VideoInfo>,
        /// Negotiated src (RGBA `W×H` GLMemory) video info, for the output GL frame map.
        out_info: Option<VideoInfo>,
        /// The unpack shader, compiled lazily once the GL context exists.
        /// Reset to `None` on each `set_caps` so the correct dialect/sampler variant is rebuilt.
        shader: Option<GLShader>,
        /// GL texture target for the *input* (sink) textures: `GL_TEXTURE_2D` (Linux glupload,
        /// `--glunpack-validate`) or `GL_TEXTURE_RECTANGLE` (macOS vtdec_hw IOSurface textures).
        /// Defaults to `GL_TEXTURE_2D`; updated by `set_caps`.
        tex_target: u32,
    }

    impl Default for State {
        fn default() -> Self {
            State {
                in_info: None,
                out_info: None,
                shader: None,
                tex_target: GL_TEXTURE_2D,
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Avc444Unpack {
        const NAME: &'static str = "RmngAvc444Unpack";
        type Type = super::Avc444Unpack;
        type ParentType = gst_gl::GLFilter;
    }

    impl ObjectImpl for Avc444Unpack {}
    impl GstObjectImpl for Avc444Unpack {}

    impl ElementImpl for Avc444Unpack {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static M: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
            Some(M.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "RMNG AVC444 GL unpacker",
                    "Filter/Converter/Video/GL",
                    "Reconstructs W×H RGBA from a decoded AVC444 stacked W×2H NV12 frame on the GPU \
                     (zero-copy GL)",
                    "RMNG",
                )
            }))
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static T: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
            T.get_or_init(|| {
                let range = || gst::IntRange::<i32>::new(1, i32::MAX);
                // Sink: NV12 W×2H GLMemory. Accepts 2D (Linux glupload / validate harness) and
                // rectangle (macOS vtdec_hw IOSurface — CGLTexImageIOSurface2D only accepts
                // GL_TEXTURE_RECTANGLE, so vtdec always emits texture-target=rectangle on macOS).
                let sink_caps = gst::Caps::builder("video/x-raw")
                    .features(["memory:GLMemory"])
                    .field("format", "NV12")
                    .field("width", range())
                    .field("height", range())
                    .field("texture-target", gst::List::new(["2D", "rectangle"]))
                    .build();
                // Src: RGBA W×H GLMemory. Always 2D: gtk4paintablesink's GL caps demand texture-
                // target=2D (GdkGLTextureBuilder has no texture-target property).
                let src_caps = gst::Caps::builder("video/x-raw")
                    .features(["memory:GLMemory"])
                    .field("format", "RGBA")
                    .field("width", range())
                    .field("height", range())
                    .field("texture-target", "2D")
                    .build();
                vec![
                    gst::PadTemplate::new("sink", gst::PadDirection::Sink, gst::PadPresence::Always, &sink_caps)
                        .unwrap(),
                    gst::PadTemplate::new("src", gst::PadDirection::Src, gst::PadPresence::Always, &src_caps)
                        .unwrap(),
                ]
            })
            .as_slice()
        }
    }

    // The output is half as tall and a different format; never in place. We deliberately DON'T
    // override transform/transform_caps/set_caps/unit_size here — GLFilter's C implementations run
    // (the Rust defaults chain up to them), and they call our GLFilterImpl methods + allocate the
    // output GL texture from a GstGLBufferPool + propagate the GL context.
    impl BaseTransformImpl for Avc444Unpack {
        const MODE: BaseTransformMode = BaseTransformMode::NeverInPlace;
        const PASSTHROUGH_ON_SAME_CAPS: bool = false;
        const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;
    }

    impl GLBaseFilterImpl for Avc444Unpack {}

    impl GLFilterImpl for Avc444Unpack {
        // Two NV12 input planes feed one RGBA output, so we need the raw 2-plane buffer, not the
        // single-texture `filter_texture` helper.
        const MODE: GLFilterMode = GLFilterMode::Buffer;
        // Our pad_templates() above are the contract (NV12 in / RGBA out); don't add GLFilter's
        // default RGBA↔RGBA templates.
        const ADD_RGBA_PAD_TEMPLATES: bool = false;

        // sink (NV12 W×2H GLMemory) ↔ src (RGBA W×H GLMemory): swap format, scale height by 2.
        // GLFilter's transform_caps wraps this (adds the memory:GLMemory feature + intersects with
        // the peer filter), so we just produce the format/size translation.
        //
        // Direction sink→src: output is always RGBA 2D (gtk4paintablesink demands 2D).
        // Direction src→sink: we can accept either 2D or rectangle on the sink.
        fn transform_internal_caps(
            &self,
            direction: gst::PadDirection,
            caps: &gst::Caps,
            filter: Option<&gst::Caps>,
        ) -> Option<gst::Caps> {
            let to_rgba = matches!(direction, gst::PadDirection::Sink); // sink NV12 in → src RGBA out
            let mut out = gst::Caps::new_empty();
            {
                let out = out.get_mut().unwrap();
                for i in 0..caps.size() {
                    let mut s = caps.structure(i).unwrap().to_owned();
                    if to_rgba {
                        // Sink → Src: output is RGBA W×H, always 2D.
                        scale_height(&mut s, 1, 2);
                        s.set("format", "RGBA");
                        s.set("texture-target", "2D");
                    } else {
                        // Src → Sink: input can be NV12 W×2H with either 2D or rectangle.
                        scale_height(&mut s, 2, 1);
                        s.set("format", "NV12");
                        s.set("texture-target", gst::List::new(["2D", "rectangle"]));
                    }
                    out.append_structure_full(s, Some(gst::CapsFeatures::new(["memory:GLMemory"])));
                }
            }
            match filter {
                Some(f) => Some(f.intersect_with_mode(&out, gst::CapsIntersectMode::First)),
                None => Some(out),
            }
        }

        fn set_caps(&self, incaps: &gst::Caps, outcaps: &gst::Caps) -> Result<(), gst::LoggableError> {
            let in_info =
                VideoInfo::from_caps(incaps).map_err(|_| gst::loggable_error!(cat(), "bad input caps"))?;
            let out_info =
                VideoInfo::from_caps(outcaps).map_err(|_| gst::loggable_error!(cat(), "bad output caps"))?;
            if in_info.width() % 2 != 0 || in_info.height() % 2 != 0 {
                return Err(gst::loggable_error!(cat(), "AVC444 unpack needs even dimensions"));
            }
            // Parse negotiated texture-target from incaps so unpack_render binds the right target.
            // VideoInfo does not carry texture-target; read it directly from the caps structure.
            let tex_target = incaps
                .structure(0)
                .and_then(|s| s.get::<&str>("texture-target").ok())
                .map(|t| if t == "rectangle" { GL_TEXTURE_RECTANGLE } else { GL_TEXTURE_2D })
                .unwrap_or(GL_TEXTURE_2D);
            let mut st = self.state.lock().unwrap();
            // Drop the cached shader: the new caps may need a different dialect or sampler variant
            // (e.g. GLES→desktop, 2D→rectangle).
            st.shader = None;
            st.in_info = Some(in_info);
            st.out_info = Some(out_info);
            st.tex_target = tex_target;
            Ok(())
        }

        // Called already on the element's GL thread (context current); input GL-sync-meta already
        // waited, output sync point set after we return. Bind the two NV12 input textures, FBO-
        // render the unpack shader into the RGBA output texture, glFlush.
        fn filter(&self, input: &gst::Buffer, output: &gst::Buffer) -> Result<(), gst::LoggableError> {
            let mut st = self.state.lock().unwrap();
            let in_info = st.in_info.clone().ok_or_else(|| gst::loggable_error!(cat(), "no caps set"))?;
            let out_info = st.out_info.clone().ok_or_else(|| gst::loggable_error!(cat(), "no caps set"))?;
            let tex_target = st.tex_target;
            let (w, h2) = (in_info.width() as i32, in_info.height() as i32); // decoded NV12 W×2H
            let h = h2 / 2; // reconstructed image height

            let ctx = self
                .obj()
                .gl_context()
                .ok_or_else(|| gst::loggable_error!(cat(), "no GL context"))?;

            if st.shader.is_none() {
                let use_rect = tex_target == GL_TEXTURE_RECTANGLE;
                st.shader = Some(build_unpack_shader(&ctx, use_rect)?);
            }
            let shader = st.shader.as_ref().unwrap().clone();
            drop(st);

            // Input: the decoded NV12 as 2 GL textures (plane 0 = Y R8 W×2H, plane 1 = UV RG8 W/2×H).
            // Mapping READ|GL holds the textures valid for the render.
            let in_frame = GLVideoFrameRef::from_buffer_ref_readable(input.as_ref(), &in_info)
                .map_err(|_| gst::loggable_error!(cat(), "map input NV12"))?;
            let y_tex = in_frame.memory(0).map_err(|_| gst::loggable_error!(cat(), "Y plane"))?.texture_id();
            let uv_tex = in_frame.memory(1).map_err(|_| gst::loggable_error!(cat(), "UV plane"))?.texture_id();

            // Output: map the RGBA GL texture WRITE|GL so GstGL records that we wrote it on the GPU
            // (sets NEED_DOWNLOAD), so the sink/gldownload re-read the texture instead of stale CPU
            // bytes. `output` is uniquely ours here (writable); take a &mut BufferRef for the map.
            let out_ref = unsafe { gst::BufferRef::from_mut_ptr(output.as_mut_ptr()) };
            let out_frame = GLVideoFrameRef::from_buffer_ref_writable(out_ref, &out_info)
                .map_err(|_| gst::loggable_error!(cat(), "map output RGBA"))?;
            let out_tex = out_frame.memory(0).map_err(|_| gst::loggable_error!(cat(), "RGBA plane"))?.texture_id();

            let ok = unsafe { unpack_render(&ctx, y_tex, uv_tex, out_tex, w, h, &shader, tex_target) };
            // Drop the output map first → commits the WRITE|GL transfer state for downstream.
            drop(out_frame);
            drop(in_frame);
            if ok {
                Ok(())
            } else {
                Err(gst::loggable_error!(cat(), "GL unpack render failed"))
            }
        }
    }

    /// Scale a structure's `height` field (fixed int or range) by `mul/div`, clamped ≥1.
    fn scale_height(s: &mut gst::Structure, mul: i64, div: i64) {
        let clamp = |v: i64| v.clamp(1, i32::MAX as i64) as i32;
        if let Ok(hv) = s.get::<i32>("height") {
            s.set("height", clamp(hv as i64 * mul / div));
        } else if let Ok(r) = s.get::<gst::IntRange<i32>>("height") {
            s.set(
                "height",
                gst::IntRange::<i32>::new(clamp(r.min() as i64 * mul / div), clamp(r.max() as i64 * mul / div)),
            );
        }
    }
}

glib::wrapper! {
    pub struct Avc444Unpack(ObjectSubclass<imp::Avc444Unpack>)
        @extends gst_gl::GLFilter, gst_gl::GLBaseFilter, gst_base::BaseTransform, gst::Element, gst::Object;
}

// ---- raw GL: FBO-render the unpack shader from the two NV12 textures into the RGBA output -------

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_RECTANGLE: u32 = 0x84F5; // IOSurface textures on macOS; also available on Mesa
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_TEXTURE1: u32 = 0x84C1;
const GL_TRIANGLES: u32 = 0x0004;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_NEAREST: i32 = 0x2600;
const GL_NO_ERROR: u32 = 0;

type GlGenFramebuffers = unsafe extern "C" fn(n: i32, fbs: *mut u32);
type GlBindFramebuffer = unsafe extern "C" fn(target: u32, fb: u32);
type GlFramebufferTexture2D = unsafe extern "C" fn(target: u32, attachment: u32, textarget: u32, tex: u32, level: i32);
type GlCheckFramebufferStatus = unsafe extern "C" fn(target: u32) -> u32;
type GlDeleteFramebuffers = unsafe extern "C" fn(n: i32, fbs: *const u32);
type GlViewport = unsafe extern "C" fn(x: i32, y: i32, w: i32, h: i32);
type GlActiveTexture = unsafe extern "C" fn(unit: u32);
type GlBindTexture = unsafe extern "C" fn(target: u32, texture: u32);
type GlTexParameteri = unsafe extern "C" fn(target: u32, pname: u32, param: i32);
type GlDrawArrays = unsafe extern "C" fn(mode: u32, first: i32, count: i32);
type GlGenVertexArrays = unsafe extern "C" fn(n: i32, arrays: *mut u32);
type GlBindVertexArray = unsafe extern "C" fn(array: u32);
type GlDeleteVertexArrays = unsafe extern "C" fn(n: i32, arrays: *const u32);
type GlGetError = unsafe extern "C" fn() -> u32;
type GlFlush = unsafe extern "C" fn();

unsafe fn gl_fn<T>(ctx: &gst_gl::GLContext, name: &str) -> Option<T> {
    let p = ctx.proc_address(name);
    if p == 0 { None } else { Some(unsafe { std::mem::transmute_copy::<usize, T>(&p) }) }
}

/// Build the unpack shader for the given GL context and input texture target.
///
/// Two axes:
/// 1. *GLSL dialect*: detected from `ctx.gl_api()` — `OPENGL3` → `#version 330 core` (macOS CGL /
///    Mesa desktop); `GLES2` → `#version 300 es` + `precision` qualifiers (Linux).
///    GstGL does **not** mangle custom GLFilter shaders, so we embed the version line in the source
///    and pass matching metadata to `GLSLStage::with_strings`.
/// 2. *Sampler type*: `use_rect=true` → `sampler2DRect` with unnormalized texel coordinates (the
///    lum()/chr() helpers compute the texel position first; the rect variant just drops the divide
///    and keeps the +0.5 center offset). `use_rect=false` → `sampler2D` with normalized coords.
///
/// Must run on the GL thread (GLSL attach needs the context) — `filter()` already is.
fn build_unpack_shader(ctx: &gst_gl::GLContext, use_rect: bool) -> Result<GLShader, gst::LoggableError> {
    use gst_gl::GLAPI;
    let is_desktop = ctx.gl_api().contains(GLAPI::OPENGL3);

    // Vertex shader: version header + body (body is dialect-identical).
    let vert_src = format!(
        "{}\n{VERT_BODY}",
        if is_desktop { "#version 330 core" } else { "#version 300 es" }
    );

    // Fragment shader header: desktop drops the precision qualifiers (legal no-ops in core GLSL
    // but not required; skipping them keeps the source clean and avoids any driver quirks).
    let frag_header = if is_desktop {
        "#version 330 core".to_string()
    } else {
        "#version 300 es\nprecision highp float;\nprecision highp int;".to_string()
    };

    // Sampler type and coordinate expressions.
    // 2D path: normalized coords — divide by texture dimensions.
    // Rect path: unnormalized texel coords — drop the divide; keep the +0.5 texel-center offset.
    let (sampler, lum_coord, chr_coord) = if use_rect {
        (
            "sampler2DRect",
            "vec2(sx + 0.5, sy + 0.5)",
            "vec2(cx + 0.5, cy + 0.5)",
        )
    } else {
        (
            "sampler2D",
            "vec2((sx + 0.5) / w, (sy + 0.5) / (2.0 * h))",
            "vec2((cx + 0.5) / (0.5 * w), (cy + 0.5) / h)",
        )
    };

    let frag_src = format!("{frag_header}\n{FRAG_BODY}")
        .replace("{SAMPLER}", sampler)
        .replace("{LUM}", lum_coord)
        .replace("{CHR}", chr_coord);

    // GLSLVersion/GLSLProfile metadata passed to GstGL: matches the embedded version line.
    // GstGL does NOT prepend a version directive from these — they are bookkeeping only.
    let (glsl_version, glsl_profile) = if is_desktop {
        (GLSLVersion::_330, GLSLProfile::CORE)
    } else {
        (GLSLVersion::None, GLSLProfile::ES)
    };

    let shader = GLShader::new(ctx);
    let v = GLSLStage::with_strings(ctx, GL_VERTEX_SHADER, glsl_version, glsl_profile, &[&vert_src]);
    shader.compile_attach_stage(&v).map_err(|e| gst::loggable_error!(cat(), "unpack vert: {e}"))?;
    let f = GLSLStage::with_strings(ctx, GL_FRAGMENT_SHADER, glsl_version, glsl_profile, &[&frag_src]);
    shader.compile_attach_stage(&f).map_err(|e| gst::loggable_error!(cat(), "unpack frag: {e}"))?;
    shader.link().map_err(|e| gst::loggable_error!(cat(), "unpack link: {e}"))?;
    Ok(shader)
}

/// FBO-render `shader` into `out_tex` (RGBA `w×h`), sampling the Y (`y_tex`, R8 `w×2h`) and UV
/// (`uv_tex`, RG8 `w/2×h`) NV12 textures. `tex_target` is the GL target for the *input* textures:
/// `GL_TEXTURE_2D` (Linux / validate harness) or `GL_TEXTURE_RECTANGLE` (macOS vtdec_hw).
/// The FBO color attachment (output) always stays `GL_TEXTURE_2D`.
///
/// **Must run on the GL thread with the context current.**
unsafe fn unpack_render(
    ctx: &gst_gl::GLContext,
    y_tex: u32,
    uv_tex: u32,
    out_tex: u32,
    w: i32,
    h: i32,
    shader: &GLShader,
    tex_target: u32,
) -> bool {
    unsafe {
        let (
            Some(gen_fb),
            Some(bind_fb),
            Some(fb_tex),
            Some(fb_status),
            Some(del_fb),
            Some(viewport),
            Some(active_tex),
            Some(bind_tex),
            Some(tex_param),
            Some(draw),
            Some(flush),
        ) = (
            gl_fn::<GlGenFramebuffers>(ctx, "glGenFramebuffers"),
            gl_fn::<GlBindFramebuffer>(ctx, "glBindFramebuffer"),
            gl_fn::<GlFramebufferTexture2D>(ctx, "glFramebufferTexture2D"),
            gl_fn::<GlCheckFramebufferStatus>(ctx, "glCheckFramebufferStatus"),
            gl_fn::<GlDeleteFramebuffers>(ctx, "glDeleteFramebuffers"),
            gl_fn::<GlViewport>(ctx, "glViewport"),
            gl_fn::<GlActiveTexture>(ctx, "glActiveTexture"),
            gl_fn::<GlBindTexture>(ctx, "glBindTexture"),
            gl_fn::<GlTexParameteri>(ctx, "glTexParameteri"),
            gl_fn::<GlDrawArrays>(ctx, "glDrawArrays"),
            gl_fn::<GlFlush>(ctx, "glFlush"),
        )
        else {
            return false;
        };
        let get_error = gl_fn::<GlGetError>(ctx, "glGetError");
        // A bound VAO is mandatory for any draw on a desktop-GL core context (the viewer's GTK/EGL
        // context, unlike the encoder's GLES context). Attributeless (gl_VertexID) still needs it.
        let vao_fns = (
            gl_fn::<GlGenVertexArrays>(ctx, "glGenVertexArrays"),
            gl_fn::<GlBindVertexArray>(ctx, "glBindVertexArray"),
            gl_fn::<GlDeleteVertexArrays>(ctx, "glDeleteVertexArrays"),
        );

        let mut vao = 0u32;
        if let (Some(gen_va), Some(bind_va), _) = vao_fns {
            gen_va(1, &mut vao);
            bind_va(vao);
        }

        let mut fb = 0u32;
        gen_fb(1, &mut fb);
        bind_fb(GL_FRAMEBUFFER, fb);
        // Output texture is always 2D (gtk4paintablesink demands it; our FBO render produces it).
        fb_tex(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, out_tex, 0);
        let status = fb_status(GL_FRAMEBUFFER);
        viewport(0, 0, w, h);

        shader.use_();
        shader.set_uniform_1i("y_tex", 0);
        shader.set_uniform_1i("uv_tex", 1);
        shader.set_uniform_1f("w", w as f32);
        shader.set_uniform_1f("h", h as f32);

        // Bind input textures to the negotiated target (GL_TEXTURE_2D or GL_TEXTURE_RECTANGLE).
        active_tex(GL_TEXTURE0);
        bind_tex(tex_target, y_tex);
        tex_param(tex_target, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        tex_param(tex_target, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        active_tex(GL_TEXTURE1);
        bind_tex(tex_target, uv_tex);
        tex_param(tex_target, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        tex_param(tex_target, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

        draw(GL_TRIANGLES, 0, 3);

        let err = get_error.map(|f| f()).unwrap_or(GL_NO_ERROR);
        if status != GL_FRAMEBUFFER_COMPLETE || err != GL_NO_ERROR {
            eprintln!(
                "[rmngavc444unpack] render issue: out_tex={out_tex} {w}x{h} \
                 fbo_status=0x{status:04x} (want 0x{GL_FRAMEBUFFER_COMPLETE:04x}) gl_err=0x{err:04x}"
            );
        }

        // Restore the GL state we touched so we don't disturb GstGL's bookkeeping.
        // Unbind input textures from the negotiated target.
        active_tex(GL_TEXTURE1);
        bind_tex(tex_target, 0);
        active_tex(GL_TEXTURE0);
        bind_tex(tex_target, 0);
        fb_tex(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 0, 0);
        bind_fb(GL_FRAMEBUFFER, 0);
        del_fb(1, &fb);
        if let (Some(_), Some(bind_va), Some(del_va)) = vao_fns {
            bind_va(0);
            del_va(1, &vao);
        }
        // Submit our GL commands so the output buffer's GstGLSyncMeta (set by GLFilter after this
        // returns) is a meaningful fence: gtk4paintablesink waits that meta in its own GL context
        // before sampling, so cross-context ordering holds WITHOUT a full CPU↔GPU stall. We used to
        // glFinish() here while hunting the "old frame on drag" bug, but that was GTK's GSK renderer
        // caching the recycled GdkTexture (fixed by GSK_RENDERER=gl in the viewer's main()) — the
        // finish was never the fix, just a heavy stall that serialized decode and display. glFlush
        // lets the two pipeline instead, cutting per-frame latency.
        flush();
        // Gate only on FBO completeness (our own object); `glGetError` can surface unrelated
        // pre-existing GstGL state, so it's logged above but not failed on.
        status == GL_FRAMEBUFFER_COMPLETE
    }
}

/// Register the `rmngavc444unpack` GL unpacker in-process (idempotent). Call once before building
/// a pipeline that references it by name.
pub fn register() -> anyhow::Result<()> {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    let mut result = Ok(());
    REGISTER.call_once(|| {
        result = gst::Element::register(None, "rmngavc444unpack", gst::Rank::NONE, Avc444Unpack::static_type())
            .map_err(|e| anyhow::anyhow!("register rmngavc444unpack: {e}"));
    });
    result
}

/// Offline pixel-correctness check (`rmng-viewer --glunpack-validate [W H]`): synthesize a `W×H`
/// Y444 frame with full-resolution 1px chroma stripes (a 4:2:0 killer), pack it to a stacked
/// `W×2H` NV12, run it through `appsrc ! glupload ! rmngavc444unpack ! gldownload ! appsink`, and
/// assert the GPU result matches [`wire::avc444::unpack_stacked_nv12_to_rgba`] (the CPU oracle) on
/// the **same bytes** — no H.264 in the loop, so this isolates the shader's gather + matrix. The
/// `gldownload` is only here, for the readback; the production path stays on the GPU.
///
/// This validates the **2D path only** — `glupload` from raw sysmem always yields 2D textures, so
/// the harness cannot reach the rectangle-sampler variant. That is the variant macOS actually uses
/// in Yuv444 (vtdec_hw emits IOSurface-backed rectangle textures), and it is covered only by
/// running live against a server in `RMNG_CHROMA=yuv444`. Exercising it here would mean building a
/// rectangle-texture source by hand; worth doing if the rect path ever regresses.
pub fn validate(w: usize, h: usize) -> anyhow::Result<()> {
    use anyhow::{Context, anyhow};
    use gstreamer_app::{AppSink, AppSrc};
    use gstreamer_video::VideoFrameRef;
    use gstreamer_video::prelude::VideoFrameExt;

    assert!(w % 2 == 0 && h % 2 == 0, "even dims only");
    register()?;
    println!("glunpack validate: {w}x{h} (stacked {w}x{})", h * 2);

    // Synthesize a Y444 frame with chroma detail a 4:2:0 encode would destroy.
    let (mut y, mut cb, mut cr) = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
    for j in 0..h {
        for i in 0..w {
            y[j * w + i] = (16 + (i * 219 / w)) as u8;
            cb[j * w + i] = if i % 2 == 0 { 80 } else { 200 };
            cr[j * w + i] = if j % 2 == 0 { 200 } else { 90 };
        }
    }
    let packed = wire::avc444::pack_y444_to_stacked_nv12(&y, &cb, &cr, w, h, w, w);

    // CPU oracle on the same bytes (luma rows 0..2H, then interleaved chroma).
    let (luma, chroma) = packed.split_at(w * 2 * h);
    let oracle = wire::avc444::unpack_stacked_nv12_to_rgba(luma, w, chroma, w, w, h);

    // GPU: upload the raw stacked NV12, unpack on the GPU, download RGBA for comparison.
    let pipeline = gst::parse::launch(
        "appsrc name=src is-live=false format=time ! glupload ! rmngavc444unpack ! \
         gldownload ! video/x-raw,format=RGBA ! appsink name=out sync=false max-buffers=4",
    )?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow!("not a pipeline"))?;
    if let Some(bus) = pipeline.bus() {
        bus.set_sync_handler(|_, msg| {
            if let gst::MessageView::Error(e) = msg.view() {
                eprintln!("glunpack validate pipeline error: {} ({:?})", e.error(), e.debug());
            }
            gst::BusSyncReply::Pass
        });
    }
    let src: AppSrc = pipeline.by_name("src").context("src")?.downcast().map_err(|_| anyhow!("appsrc"))?;
    let sink: AppSink = pipeline.by_name("out").context("out")?.downcast().map_err(|_| anyhow!("appsink"))?;
    src.set_caps(Some(
        &gst::Caps::builder("video/x-raw")
            .field("format", "NV12")
            .field("width", w as i32)
            .field("height", (h * 2) as i32)
            .field("framerate", gst::Fraction::new(30, 1))
            .build(),
    ));
    pipeline.set_state(gst::State::Playing)?;
    for k in 0..4 {
        let mut buf = gst::Buffer::from_slice(packed.clone());
        buf.get_mut().unwrap().set_pts(gst::ClockTime::from_mseconds(k * 33));
        src.push_buffer(buf).map_err(|e| anyhow!("push: {e:?}"))?;
    }
    let _ = src.end_of_stream();

    let mut got = None;
    while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
        let buf = sample.buffer().context("buf")?;
        let info = VideoInfo::from_caps(sample.caps().context("caps")?)?;
        let frame = VideoFrameRef::from_buffer_ref_readable(buf, &info).map_err(|_| anyhow!("map rgba"))?;
        let stride = frame.plane_stride()[0] as usize;
        let data = frame.plane_data(0)?;
        let mut tight = vec![0u8; w * h * 4];
        for r in 0..h {
            tight[r * w * 4..r * w * 4 + w * 4].copy_from_slice(&data[r * stride..r * stride + w * 4]);
        }
        got = Some(tight);
    }
    pipeline.set_state(gst::State::Null)?;
    let gpu = got.context("no RGBA frame from GL unpack")?;

    // Compare RGB (ignore alpha) within a small tolerance for GPU rounding.
    let (mut sum, mut max, mut nbad) = (0u64, 0u8, 0u64);
    for p in 0..w * h {
        for c in 0..3 {
            let d = (gpu[p * 4 + c] as i32 - oracle[p * 4 + c] as i32).unsigned_abs() as u8;
            sum += d as u64;
            max = max.max(d);
            if d > 2 {
                nbad += 1;
            }
        }
    }
    let mean = sum as f64 / (w * h * 3) as f64;
    println!("GL unpack vs CPU oracle: mean abs RGB err={mean:.4} max={max} (#>2: {nbad})");
    if max > 4 || mean > 1.0 {
        return Err(anyhow!("GL unpack diverges from oracle (mean {mean:.3}, max {max})"));
    }
    println!("OK: GL unpack matches the oracle within tolerance.");
    Ok(())
}
