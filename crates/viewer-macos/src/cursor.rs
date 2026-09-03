//! The remote cursor: turn a `CursorShape` into a real `NSCursor` so the operator's own pointer
//! takes the remote's shape (I-beam, hand, resize…), exactly as the GTK viewer does with
//! `gdk::Cursor::from_texture`.
//!
//! The daemon captures the sprite as `SPA_META_Cursor`, which is **BGRA8888 premultiplied**
//! despite the field being named `rgba` — the GTK viewer makes the same reading
//! (`MemoryFormat::B8g8r8a8Premultiplied`). `CGBitmapInfo` says so explicitly here:
//! premultiplied-first plus little-endian 32-bit order is BGRA in memory.

use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_app_kit::{NSCursor, NSImage};
use objc2_core_foundation::CFData;
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::{NSPoint, NSSize};
use wire::socket::CursorShape;

/// Build an `NSCursor` for `shape`, hotspot included. `None`-worthy failures are returned as
/// errors so the caller can keep the previous cursor rather than losing the pointer entirely.
pub fn cursor_from_shape(shape: &CursorShape) -> Result<Retained<NSCursor>> {
    let (w, h) = (shape.width as usize, shape.height as usize);
    let need = w * h * 4;
    if w == 0 || h == 0 || shape.rgba.len() < need {
        return Err(anyhow!("bad cursor sprite {w}x{h} ({} bytes)", shape.rgba.len()));
    }
    let data = CFData::from_bytes(&shape.rgba[..need]);
    let provider =
        CGDataProvider::with_cf_data(Some(&data)).ok_or_else(|| anyhow!("CGDataProvider failed"))?;
    let space = CGColorSpace::new_device_rgb().ok_or_else(|| anyhow!("CGColorSpace failed"))?;
    // BGRA premultiplied = alpha-first + 32-bit little-endian byte order.
    let bitmap = CGBitmapInfo(
        CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
    );
    let image = unsafe {
        CGImage::new(
            w,
            h,
            8,
            32,
            w * 4,
            Some(&space),
            bitmap,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or_else(|| anyhow!("CGImageCreate failed"))?;

    let ns_image =
        NSImage::initWithCGImage_size(NSImage::alloc(), &image, NSSize::new(w as f64, h as f64));
    let hotspot = NSPoint::new(shape.hotspot_x as f64, shape.hotspot_y as f64);
    Ok(NSCursor::initWithImage_hotSpot(NSCursor::alloc(), &ns_image, hotspot))
}
