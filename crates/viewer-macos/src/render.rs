//! Metal render path.
//!
//! A decoded NV12 `CVPixelBuffer` (IOSurface-backed) becomes two zero-copy Metal textures — Y as
//! R8, CbCr as RG8 — via a `CVMetalTextureCache`. From there:
//!
//! - **4:2:0** (`ChromaMode::Yuv420`): one pass straight to the drawable, BT.709-limited
//!   YCbCr→RGB, letterboxed with the Metal viewport.
//! - **4:4:4** (`ChromaMode::Yuv444`): the stream is a double-height `W×2H` NV12 carrying the
//!   AVC444 main view stacked over an auxiliary chroma view (see [`wire::avc444`]). Pass 1
//!   gathers the polyphase chroma quadrants back into full `W×H` 4:4:4 and does the
//!   BT.601-limited matrix into an offscreen RGBA texture; pass 2 blits that to the drawable.
//!   This is the Metal twin of the GTK viewer's `rmngavc444unpack` GL filter, and
//!   `--unpack-validate` checks it against the same CPU oracle.
//!
//! The two matrices differ because the encoder does: the 4:2:0 stream is tagged BT.709 limited
//! (`VIDEO_COLORIMETRY` in the GTK viewer), while the AVC444 pack/unpack pair is defined against
//! BT.601 limited. Matching the oracle is what makes the validator meaningful, so do not
//! "harmonise" them.

use std::cell::RefCell;
use std::ptr::NonNull;

use anyhow::{anyhow, bail, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVImageBuffer, CVMetalTexture, CVMetalTextureCache};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLDevice, MTLLibrary,
    MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState,
    MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
    MTLViewport,
};
use objc2_quartz_core::CAMetalDrawable;

use crate::decoder::DecodedFrame;

/// A retained Metal texture handle.
type Tex = Retained<ProtocolObject<dyn MTLTexture>>;

/// Vertex stage plus the three fragment stages: direct 4:2:0, the AVC444 unpack, and the blit.
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VOut { float4 pos [[position]]; float2 uv; };

// VBO-less fullscreen triangle. uv is y-flipped: texture origin is top-left, NDC is bottom-left.
vertex VOut v_main(uint vid [[vertex_id]]) {
    float2 p = float2((vid << 1) & 2, vid & 2);
    VOut o;
    o.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = float2(p.x, 1.0 - p.y);
    return o;
}

// BT.709 limited-range Y'CbCr -> RGB (the 4:2:0 stream's tagged colorimetry).
static inline float3 ycbcr709_limited(float y, float cb, float cr) {
    float yy = (y - 16.0/255.0) * (255.0/219.0);
    float u  = cb - 128.0/255.0;
    float v  = cr - 128.0/255.0;
    return float3(yy + 1.5748 * v,
                  yy - 0.1873 * u - 0.4681 * v,
                  yy + 1.8556 * u);
}

// BT.601 limited-range, in 0-255 space, matching wire::avc444's CPU oracle coefficients exactly.
static inline float3 ycbcr601_limited_255(float Y, float Cb, float Cr) {
    float c = (Y - 16.0) * 1.164383;
    float d = Cb - 128.0;
    float e = Cr - 128.0;
    return float3(c + 1.596027 * e,
                  c - 0.391762 * d - 0.812968 * e,
                  c + 2.017232 * d) / 255.0;
}

// ── 4:2:0: sample both planes and convert straight to the drawable ──────────────────────────
fragment float4 f_nv12(VOut in [[stage_in]],
                       texture2d<float> yTex [[texture(0)]],
                       texture2d<float> cbcrTex [[texture(1)]]) {
    constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::linear);
    float  y    = yTex.sample(s, in.uv).r;
    float2 cbcr = cbcrTex.sample(s, in.uv).rg;
    return float4(saturate(ycbcr709_limited(y, cbcr.x, cbcr.y)), 1.0);
}

// ── 4:4:4: gather the polyphase quadrants out of the stacked W x 2H NV12 ────────────────────
//
// Layout (wire::avc444): luma rows [0,H) are the image luma; luma rows [H,2H) tile the aux luma
// as [Cb01 | Cb10 ; Cb11 | Cr01]; chroma rows [0,H/2) are the main U=Cb00,V=Cr00 and rows
// [H/2,H) the aux U=Cr10,V=Cr11. Nearest sampling at texel centres, so this is an exact gather.
fragment float4 f_unpack(VOut in [[stage_in]],
                         texture2d<float> yTex [[texture(0)]],
                         texture2d<float> cTex [[texture(1)]]) {
    constexpr sampler n(coord::normalized, address::clamp_to_edge, filter::nearest);
    uint w  = yTex.get_width();
    uint h2 = yTex.get_height();      // 2H
    uint h  = h2 / 2u;
    uint cw = cTex.get_width();       // W/2
    uint chh = cTex.get_height();     // H
    uint ch = h / 2u;

    uint px = uint(in.pos.x);
    uint py = uint(in.pos.y);
    uint i  = px & 1u, j = py & 1u, xc = px >> 1, yc = py >> 1;

    // Texel-centre lookups.
    float Y = yTex.sample(n, float2((float(px) + 0.5) / float(w),
                                    (float(py) + 0.5) / float(h2))).r * 255.0;

    float2 mainC = cTex.sample(n, float2((float(xc) + 0.5) / float(cw),
                                         (float(yc) + 0.5) / float(chh))).rg * 255.0;
    float2 auxC  = cTex.sample(n, float2((float(xc) + 0.5) / float(cw),
                                         (float(ch + yc) + 0.5) / float(chh))).rg * 255.0;

    float auxTL = yTex.sample(n, float2((float(xc) + 0.5) / float(w),
                                        (float(h + yc) + 0.5) / float(h2))).r * 255.0;
    float auxTR = yTex.sample(n, float2((float(w / 2u + xc) + 0.5) / float(w),
                                        (float(h + yc) + 0.5) / float(h2))).r * 255.0;
    float auxBL = yTex.sample(n, float2((float(xc) + 0.5) / float(w),
                                        (float(h + ch + yc) + 0.5) / float(h2))).r * 255.0;
    float auxBR = yTex.sample(n, float2((float(w / 2u + xc) + 0.5) / float(w),
                                        (float(h + ch + yc) + 0.5) / float(h2))).r * 255.0;

    float Cb, Cr;
    if (i == 0u && j == 0u)      { Cb = mainC.x; Cr = mainC.y; }   // Cb00, Cr00
    else if (i == 1u && j == 0u) { Cb = auxTL;   Cr = auxBR;   }   // Cb01, Cr01
    else if (i == 0u && j == 1u) { Cb = auxTR;   Cr = auxC.x;  }   // Cb10, Cr10
    else                         { Cb = auxBL;   Cr = auxC.y;  }   // Cb11, Cr11

    return float4(saturate(ycbcr601_limited_255(Y, Cb, Cr)), 1.0);
}

// ── blit an RGBA texture to the drawable ────────────────────────────────────────────────────
fragment float4 f_blit(VOut in [[stage_in]], texture2d<float> tex [[texture(0)]]) {
    constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::linear);
    return tex.sample(s, in.uv);
}
"#;

/// The offscreen RGBA target the 4:4:4 unpack renders into, cached by size.
struct Offscreen {
    w: usize,
    h: usize,
    tex: Tex,
}

pub struct Renderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// 4:2:0 NV12 → drawable (BGRA).
    pipe_nv12: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    /// AVC444 stacked NV12 → offscreen RGBA8.
    pipe_unpack: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    /// RGBA texture → drawable (BGRA).
    pipe_blit: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    cache: CFRetained<CVMetalTextureCache>,
    offscreen: RefCell<Option<Offscreen>>,
}

impl Renderer {
    /// Build the renderer on the system default Metal device.
    pub fn new() -> Result<Self> {
        let device =
            objc2_metal::MTLCreateSystemDefaultDevice().ok_or_else(|| anyhow!("no Metal device"))?;
        Self::with_device(device)
    }

    fn with_device(device: Retained<ProtocolObject<dyn MTLDevice>>) -> Result<Self> {
        let queue = device.newCommandQueue().ok_or_else(|| anyhow!("newCommandQueue failed"))?;
        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADER), None)
            .map_err(|e| anyhow!("shader compile failed: {e:?}"))?;
        let pipe_nv12 = pipeline(&device, &library, "f_nv12", MTLPixelFormat::BGRA8Unorm)?;
        let pipe_unpack = pipeline(&device, &library, "f_unpack", MTLPixelFormat::RGBA8Unorm)?;
        let pipe_blit = pipeline(&device, &library, "f_blit", MTLPixelFormat::BGRA8Unorm)?;

        let mut cache: *mut CVMetalTextureCache = std::ptr::null_mut();
        let status = unsafe {
            CVMetalTextureCacheCreate(
                std::ptr::null(),
                std::ptr::null(),
                &device,
                std::ptr::null(),
                NonNull::new(&mut cache).unwrap(),
            )
        };
        if status != 0 || cache.is_null() {
            bail!("CVMetalTextureCacheCreate failed: {status}");
        }
        let cache = unsafe { CFRetained::from_raw(NonNull::new(cache).unwrap()) };

        Ok(Renderer {
            device,
            queue,
            pipe_nv12,
            pipe_unpack,
            pipe_blit,
            cache,
            offscreen: RefCell::new(None),
        })
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    /// Render `frame` into `drawable`, letterboxed inside `drawable_w × drawable_h` (physical
    /// pixels). A `None` frame just clears to the letterbox black.
    pub fn draw(
        &self,
        frame: Option<&DecodedFrame>,
        drawable: &ProtocolObject<dyn CAMetalDrawable>,
        drawable_w: f64,
        drawable_h: f64,
    ) -> Result<()> {
        let cmd = self.queue.commandBuffer().ok_or_else(|| anyhow!("no command buffer"))?;

        // 4:4:4 needs a reconstruction pass into an offscreen RGBA target first.
        let unpacked: Option<Tex> = match frame {
            Some(f) if f.yuv444 => {
                let (y, c) = self.plane_textures(f)?;
                let off = self.ensure_offscreen(f.width, f.height)?;
                let pass = render_pass(&off, MTLLoadAction::DontCare);
                let enc = cmd
                    .renderCommandEncoderWithDescriptor(&pass)
                    .ok_or_else(|| anyhow!("no unpack encoder"))?;
                enc.setRenderPipelineState(&self.pipe_unpack);
                unsafe {
                    enc.setFragmentTexture_atIndex(Some(&y), 0);
                    enc.setFragmentTexture_atIndex(Some(&c), 1);
                    enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
                }
                enc.endEncoding();
                Some(off)
            }
            _ => None,
        };

        let target = drawable.texture();
        let pass = render_pass(&target, MTLLoadAction::Clear);
        let enc = cmd
            .renderCommandEncoderWithDescriptor(&pass)
            .ok_or_else(|| anyhow!("no render encoder"))?;
        if let Some(f) = frame {
            // Letterbox: uniform scale to fit, centred; the rest stays cleared black.
            let (fw, fh) = (f.width as f64, f.height as f64);
            let scale = (drawable_w / fw).min(drawable_h / fh);
            let (vw, vh) = (fw * scale, fh * scale);
            enc.setViewport(MTLViewport {
                originX: (drawable_w - vw) / 2.0,
                originY: (drawable_h - vh) / 2.0,
                width: vw,
                height: vh,
                znear: 0.0,
                zfar: 1.0,
            });
            match &unpacked {
                Some(rgba) => {
                    enc.setRenderPipelineState(&self.pipe_blit);
                    unsafe { enc.setFragmentTexture_atIndex(Some(rgba), 0) };
                }
                None => {
                    let (y, c) = self.plane_textures(f)?;
                    enc.setRenderPipelineState(&self.pipe_nv12);
                    unsafe {
                        enc.setFragmentTexture_atIndex(Some(&y), 0);
                        enc.setFragmentTexture_atIndex(Some(&c), 1);
                    }
                }
            }
            unsafe { enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3) };
        }
        enc.endEncoding();
        cmd.presentDrawable(ProtocolObject::from_ref(drawable));
        cmd.commit();
        Ok(())
    }

    /// The cached `w×h` RGBA offscreen target, rebuilt when the size changes.
    fn ensure_offscreen(&self, w: usize, h: usize) -> Result<Tex> {
        let mut slot = self.offscreen.borrow_mut();
        if let Some(o) = slot.as_ref() {
            if o.w == w && o.h == h {
                return Ok(o.tex.clone());
            }
        }
        let tex = new_texture(
            &self.device,
            MTLPixelFormat::RGBA8Unorm,
            w,
            h,
            MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
            MTLStorageMode::Shared,
        )?;
        *slot = Some(Offscreen { w, h, tex: tex.clone() });
        Ok(tex)
    }

    /// Y (R8) and CbCr (RG8) textures for `frame`'s pixel buffer, zero-copy via the cache.
    fn plane_textures(&self, frame: &DecodedFrame) -> Result<(Tex, Tex)> {
        let pb: &CVImageBuffer = &frame.pixel_buffer;
        // The decoded buffer is W×2H for 4:4:4, W×H otherwise; chroma is half in both axes.
        let buf_h = if frame.yuv444 { frame.height * 2 } else { frame.height };
        let w = frame.width;
        let y = self.plane_texture(pb, MTLPixelFormat::R8Unorm, w, buf_h, 0)?;
        let cbcr = self.plane_texture(pb, MTLPixelFormat::RG8Unorm, w / 2, buf_h / 2, 1)?;
        Ok((y, cbcr))
    }

    fn plane_texture(
        &self,
        pb: &CVImageBuffer,
        format: MTLPixelFormat,
        w: usize,
        h: usize,
        plane: usize,
    ) -> Result<Tex> {
        let mut cvtex: *mut CVMetalTexture = std::ptr::null_mut();
        let status = unsafe {
            CVMetalTextureCacheCreateTextureFromImage(
                std::ptr::null(),
                &self.cache,
                pb,
                std::ptr::null(),
                format.0,
                w,
                h,
                plane,
                NonNull::new(&mut cvtex).unwrap(),
            )
        };
        if status != 0 || cvtex.is_null() {
            bail!("CVMetalTextureCacheCreateTextureFromImage(plane {plane}) failed: {status}");
        }
        let cvtex = unsafe { CFRetained::from_raw(NonNull::new(cvtex).unwrap()) };
        let mtl = unsafe { CVMetalTextureGetTexture(&cvtex) };
        if mtl.is_null() {
            bail!("CVMetalTextureGetTexture returned null");
        }
        // The MTLTexture is +0 borrowed from the CVMetalTexture we drop here, so retain it.
        unsafe { Retained::retain(mtl) }.ok_or_else(|| anyhow!("retain MTLTexture failed"))
    }
}

/// One-colour-attachment render pass onto `target`.
fn render_pass(
    target: &ProtocolObject<dyn MTLTexture>,
    load: MTLLoadAction,
) -> Retained<MTLRenderPassDescriptor> {
    let pass = MTLRenderPassDescriptor::new();
    let att = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
    att.setTexture(Some(target));
    att.setLoadAction(load);
    att.setStoreAction(MTLStoreAction::Store);
    att.setClearColor(MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 1.0 });
    pass
}

/// Compile one `v_main` + `fragment` pipeline writing `format`.
fn pipeline(
    device: &ProtocolObject<dyn MTLDevice>,
    library: &ProtocolObject<dyn MTLLibrary>,
    fragment: &str,
    format: MTLPixelFormat,
) -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>> {
    let vfn = library
        .newFunctionWithName(&NSString::from_str("v_main"))
        .ok_or_else(|| anyhow!("missing v_main"))?;
    let ffn = library
        .newFunctionWithName(&NSString::from_str(fragment))
        .ok_or_else(|| anyhow!("missing {fragment}"))?;
    let desc = MTLRenderPipelineDescriptor::new();
    desc.setVertexFunction(Some(&vfn));
    desc.setFragmentFunction(Some(&ffn));
    unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) }.setPixelFormat(format);
    device
        .newRenderPipelineStateWithDescriptor_error(&desc)
        .map_err(|e| anyhow!("pipeline {fragment} build failed: {e:?}"))
}

/// Allocate a 2D texture.
fn new_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    format: MTLPixelFormat,
    w: usize,
    h: usize,
    usage: MTLTextureUsage,
    storage: MTLStorageMode,
) -> Result<Tex> {
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            format, w, h, false,
        )
    };
    desc.setUsage(usage);
    desc.setStorageMode(storage);
    device.newTextureWithDescriptor(&desc).ok_or_else(|| anyhow!("newTextureWithDescriptor failed"))
}

/// `--unpack-validate W H`: render the Metal AVC444 unpack over a synthetic packed frame and
/// compare it pixel-for-pixel with [`wire::avc444::unpack_stacked_nv12_to_rgba`], the CPU oracle
/// the GTK viewer's GL filter is held to. Needs a GPU but no window and no server.
pub fn validate_unpack(w: usize, h: usize) -> Result<()> {
    if w % 2 != 0 || h % 2 != 0 {
        bail!("--unpack-validate needs even dimensions (got {w}x{h})");
    }
    println!("unpack validate: {w}x{h} (stacked {w}x{})", 2 * h);

    // Deterministic synthetic Y/Cb/Cr, packed exactly as the encoder packs.
    let fill = |buf: &mut [u8], seed: u64| {
        let mut s = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        for b in buf.iter_mut() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *b = (s & 0xFF) as u8;
        }
    };
    let (mut y, mut cb, mut cr) = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
    fill(&mut y, 1);
    fill(&mut cb, 2);
    fill(&mut cr, 3);
    let packed = wire::avc444::pack_y444_to_stacked_nv12(&y, &cb, &cr, w, h, w, w);
    let (luma, chroma) = packed.split_at(w * 2 * h);

    let device =
        objc2_metal::MTLCreateSystemDefaultDevice().ok_or_else(|| anyhow!("no Metal device"))?;
    let r = Renderer::with_device(device)?;

    // Upload the packed planes as the same two textures the decode path produces.
    let ytex = new_texture(
        &r.device,
        MTLPixelFormat::R8Unorm,
        w,
        2 * h,
        MTLTextureUsage::ShaderRead,
        MTLStorageMode::Shared,
    )?;
    upload(&ytex, luma, w, w, 2 * h);
    let ctex = new_texture(
        &r.device,
        MTLPixelFormat::RG8Unorm,
        w / 2,
        h,
        MTLTextureUsage::ShaderRead,
        MTLStorageMode::Shared,
    )?;
    upload(&ctex, chroma, w, w / 2, h);

    // Run the unpack pass into the offscreen RGBA target.
    let out = r.ensure_offscreen(w, h)?;
    let cmd = r.queue.commandBuffer().ok_or_else(|| anyhow!("no command buffer"))?;
    let pass = render_pass(&out, MTLLoadAction::DontCare);
    let enc =
        cmd.renderCommandEncoderWithDescriptor(&pass).ok_or_else(|| anyhow!("no encoder"))?;
    enc.setRenderPipelineState(&r.pipe_unpack);
    unsafe {
        enc.setFragmentTexture_atIndex(Some(&ytex), 0);
        enc.setFragmentTexture_atIndex(Some(&ctex), 1);
        enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
    }
    enc.endEncoding();
    cmd.commit();
    cmd.waitUntilCompleted();

    // Read back and compare against the oracle.
    let mut gpu = vec![0u8; w * h * 4];
    unsafe {
        out.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            NonNull::new(gpu.as_mut_ptr().cast()).unwrap(),
            w * 4,
            MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize { width: w, height: h, depth: 1 },
            },
            0,
        );
    }
    let oracle = wire::avc444::unpack_stacked_nv12_to_rgba(luma, w, chroma, w, w, h);

    let (mut sum, mut max, mut nbad) = (0u64, 0u8, 0usize);
    for p in 0..w * h {
        for c in 0..3 {
            let d = gpu[p * 4 + c].abs_diff(oracle[p * 4 + c]);
            sum += d as u64;
            max = max.max(d);
            if d > 2 {
                nbad += 1;
            }
        }
    }
    let mean = sum as f64 / (w * h * 3) as f64;
    println!("Metal unpack vs CPU oracle: mean abs RGB err={mean:.4} max={max} (#>2: {nbad})");
    if mean > 0.5 || max > 2 {
        bail!("Metal unpack diverges from the oracle (mean {mean:.3}, max {max})");
    }
    println!("OK: Metal unpack matches the oracle within tolerance.");
    Ok(())
}

/// Copy `src` (row stride `stride`) into `tex`.
fn upload(tex: &ProtocolObject<dyn MTLTexture>, src: &[u8], stride: usize, w: usize, h: usize) {
    unsafe {
        tex.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
            MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize { width: w, height: h, depth: 1 },
            },
            0,
            NonNull::new(src.as_ptr() as *mut std::ffi::c_void).unwrap(),
            stride,
        );
    }
}

// CoreVideo Metal-texture-cache C entry points (typed against the objc2 Metal/CV types).
unsafe extern "C-unwind" {
    fn CVMetalTextureCacheCreate(
        allocator: *const objc2_core_foundation::CFAllocator,
        cache_attributes: *const objc2_core_foundation::CFDictionary,
        metal_device: &ProtocolObject<dyn MTLDevice>,
        texture_attributes: *const objc2_core_foundation::CFDictionary,
        cache_out: NonNull<*mut CVMetalTextureCache>,
    ) -> i32;
    fn CVMetalTextureCacheCreateTextureFromImage(
        allocator: *const objc2_core_foundation::CFAllocator,
        texture_cache: &CVMetalTextureCache,
        source_image: &CVImageBuffer,
        texture_attributes: *const objc2_core_foundation::CFDictionary,
        pixel_format: usize,
        width: usize,
        height: usize,
        plane_index: usize,
        texture_out: NonNull<*mut CVMetalTexture>,
    ) -> i32;
    fn CVMetalTextureGetTexture(texture: &CVMetalTexture) -> *mut ProtocolObject<dyn MTLTexture>;
}
