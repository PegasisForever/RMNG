//! Metal render path: a decoded NV12 `CVPixelBuffer` (IOSurface-backed) → two zero-copy Metal
//! textures (Y as R8, CbCr as RG8) via a `CVMetalTextureCache` → a fullscreen-triangle shader
//! that does BT.709-limited YCbCr→RGB, letterboxed into a `CAMetalLayer` drawable.
//!
//! This is the 4:2:0 path (the live server mode). The 4:4:4 AVC444 reconstruction shader — the
//! Metal twin of `crates/viewer/src/glunpack.rs` — is added in a later milestone; until then a
//! 4:4:4 frame is rendered from its main (top-half) view, which is a valid 4:2:0 image.

use std::ptr::NonNull;

use anyhow::{anyhow, bail, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVImageBuffer, CVMetalTexture, CVMetalTextureCache};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLLibrary,
    MTLCommandEncoder, MTLRenderCommandEncoder, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLStoreAction,
    MTLTexture, MTLViewport,
};
use objc2_quartz_core::CAMetalDrawable;

use crate::decoder::DecodedFrame;

/// A retained Metal texture handle.
type Tex = Retained<ProtocolObject<dyn MTLTexture>>;

/// The Metal shading language source: a VBO-less fullscreen triangle and a BT.709-limited
/// NV12→RGB fragment shader. `v_uv` spans the *image* (letterboxing is done with the viewport,
/// so uv stays `[0,1]`).
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VOut { float4 pos [[position]]; float2 uv; };

vertex VOut v_main(uint vid [[vertex_id]]) {
    // Oversized triangle covering the viewport; uv in [0,1] with y flipped (texture origin
    // top-left, NDC origin bottom-left).
    float2 p = float2((vid << 1) & 2, vid & 2);
    VOut o;
    o.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = float2(p.x, 1.0 - p.y);
    return o;
}

fragment float4 f_main(VOut in [[stage_in]],
                       texture2d<float> yTex [[texture(0)]],
                       texture2d<float> cbcrTex [[texture(1)]]) {
    constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::linear);
    float y  = yTex.sample(s, in.uv).r;
    float2 cbcr = cbcrTex.sample(s, in.uv).rg;
    // BT.709 limited-range Y'CbCr -> RGB (matches the encoder's 2:3:7:1 colorimetry).
    float yy = (y - 16.0/255.0) * (255.0/219.0);
    float cb = cbcr.x - 128.0/255.0;
    float cr = cbcr.y - 128.0/255.0;
    float r = yy + 1.5748 * cr;
    float g = yy - 0.1873 * cb - 0.4681 * cr;
    float b = yy + 1.8556 * cb;
    return float4(saturate(float3(r, g, b)), 1.0);
}
"#;

pub struct Renderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    cache: CFRetained<CVMetalTextureCache>,
}

impl Renderer {
    /// Build the renderer on the system default Metal device.
    pub fn new() -> Result<Self> {
        let device = objc2_metal::MTLCreateSystemDefaultDevice()
            .ok_or_else(|| anyhow!("no Metal device"))?;
        let queue = device.newCommandQueue().ok_or_else(|| anyhow!("newCommandQueue failed"))?;

        let src = NSString::from_str(SHADER);
        let library = device
            .newLibraryWithSource_options_error(&src, None)
            .map_err(|e| anyhow!("shader compile failed: {e:?}"))?;
        let vfn = library
            .newFunctionWithName(&NSString::from_str("v_main"))
            .ok_or_else(|| anyhow!("missing v_main"))?;
        let ffn = library
            .newFunctionWithName(&NSString::from_str("f_main"))
            .ok_or_else(|| anyhow!("missing f_main"))?;
        let desc = MTLRenderPipelineDescriptor::new();
        desc.setVertexFunction(Some(&vfn));
        desc.setFragmentFunction(Some(&ffn));
        unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&desc)
            .map_err(|e| anyhow!("pipeline build failed: {e:?}"))?;

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

        Ok(Renderer { device, queue, pipeline, cache })
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    /// Render `frame` into `drawable`, letterboxed inside `drawable_w × drawable_h` (physical
    /// pixels). A `None` frame clears to black (the letterbox colour).
    pub fn draw(
        &self,
        frame: Option<&DecodedFrame>,
        drawable: &ProtocolObject<dyn CAMetalDrawable>,
        drawable_w: f64,
        drawable_h: f64,
    ) -> Result<()> {
        let target = drawable.texture();
        let pass = MTLRenderPassDescriptor::new();
        let att = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        att.setTexture(Some(&target));
        att.setLoadAction(MTLLoadAction::Clear);
        att.setStoreAction(MTLStoreAction::Store);
        att.setClearColor(MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 1.0 });

        let cmd = self.queue.commandBuffer().ok_or_else(|| anyhow!("no command buffer"))?;
        let enc = cmd
            .renderCommandEncoderWithDescriptor(&pass)
            .ok_or_else(|| anyhow!("no render encoder"))?;

        if let Some(frame) = frame {
            if let Ok((ytex, cbcrtex)) = self.plane_textures(frame) {
                // Letterbox: uniform scale to fit, centred, the rest stays cleared black.
                let (fw, fh) = (frame.width as f64, frame.height as f64);
                let scale = (drawable_w / fw).min(drawable_h / fh);
                let (vw, vh) = (fw * scale, fh * scale);
                let vp = MTLViewport {
                    originX: (drawable_w - vw) / 2.0,
                    originY: (drawable_h - vh) / 2.0,
                    width: vw,
                    height: vh,
                    znear: 0.0,
                    zfar: 1.0,
                };
                enc.setViewport(vp);
                enc.setRenderPipelineState(&self.pipeline);
                unsafe {
                    enc.setFragmentTexture_atIndex(Some(&ytex), 0);
                    enc.setFragmentTexture_atIndex(Some(&cbcrtex), 1);
                    enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
                }
            }
        }
        enc.endEncoding();
        cmd.presentDrawable(ProtocolObject::from_ref(drawable));
        cmd.commit();
        Ok(())
    }

    /// Create the Y (R8) and CbCr (RG8) Metal textures for `frame`'s pixel buffer, zero-copy via
    /// the texture cache. For a 4:4:4 stacked buffer we bind only the main (top-half) view for
    /// now, so plane dimensions use the *buffer* height.
    fn plane_textures(
        &self,
        frame: &DecodedFrame,
    ) -> Result<(Tex, Tex)> {
        let pb: &CVImageBuffer = &frame.pixel_buffer;
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
        // Retain the MTLTexture (+0 borrowed from the CVMetalTexture, which we drop here).
        unsafe { Retained::retain(mtl) }.ok_or_else(|| anyhow!("retain MTLTexture failed"))
    }
}

/// `--unpack-validate W H`: compare the Metal AVC444 unpack against the CPU oracle. Implemented
/// in the 4:4:4 milestone; for now it reports that it is pending so the CLI flag exists.
pub fn validate_unpack(_w: usize, _h: usize) -> Result<()> {
    bail!("--unpack-validate: the Metal AVC444 unpack shader is not implemented yet (4:4:4 milestone)")
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
