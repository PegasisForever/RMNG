//! Full-chroma (4:4:4) packing for the single **double-height** video stream.
//!
//! A hardware H.264 encoder that only does 4:2:0 still carries full 4:4:4 if we send the
//! dropped chroma in a second 4:2:0 view — RDP's AVC444 trick. RMNG carries both views in
//! **one `W×2H` NV12 frame**: the **main view** (the image's luma + a base chroma sample)
//! on top, an **auxiliary view** (the remaining chroma) on the bottom. An `W×2H` NV12 has
//! `2·W·H` luma + `W·H` interleaved-chroma bytes = `3·W·H` — exactly `Y + Cb + Cr` at full
//! resolution, so the pack is **lossless and wastes nothing** before the H.264 stage.
//!
//! Layout is a clean-room **polyphase-quadrant** scheme (simpler + exactly invertible than
//! MS-RDPEGFX's averaged-main + reverse-filter form; we never decode the main view alone, so
//! the standalone-4:2:0 quality of the main is irrelevant). For a chroma plane `C` (`w×h`),
//! split into four `w/2 × h/2` phase quadrants `Cij(x,y) = C[2x+i, 2y+j]`. Then:
//!
//! ```text
//! stacked W×2H NV12 (W=w, H=h):
//!   luma  rows [0 .. H)   = Y                      (full-res image luma)
//!   luma  rows [H .. 2H)  = aux luma, 2×2 tiling of (w/2 × h/2) blocks:
//!                             ┌──────────┬──────────┐
//!                             │  Cb01    │  Cb10    │   (top    band, rows H .. H+h/2)
//!                             ├──────────┼──────────┤
//!                             │  Cb11    │  Cr01    │   (bottom band, rows H+h/2 .. 2H)
//!                             └──────────┴──────────┘
//!   chroma rows [0 .. H/2)   = main chroma:  U=Cb00, V=Cr00   (NV12 interleaved)
//!   chroma rows [H/2 .. H)   = aux  chroma:  U=Cr10, V=Cr11
//! ```
//!
//! All 8 quadrants placed → full Cb (`Cb00/01/10/11`) + full Cr (`Cr00/01/10/11`). The GPU
//! pack/unpack shaders implement the same mapping; this module is the CPU reference + the
//! round-trip oracle the GL path is validated against.

/// Byte length of the packed stacked-NV12 buffer for a `w×h` source (tight: luma stride `w`,
/// chroma stride `w`). Luma `2·w·h` + interleaved chroma `w·h`.
pub fn stacked_nv12_len(w: usize, h: usize) -> usize {
    w * h * 3
}

/// Pack full-resolution planar Y/Cb/Cr (`w×h` each, with row strides `y_stride`/`c_stride`)
/// into a tightly-packed stacked `W×2H` NV12 buffer (luma stride `w`, chroma stride `w`).
/// `w` and `h` must be even.
pub fn pack_y444_to_stacked_nv12(
    y: &[u8],
    cb: &[u8],
    cr: &[u8],
    w: usize,
    h: usize,
    y_stride: usize,
    c_stride: usize,
) -> Vec<u8> {
    assert!(
        w % 2 == 0 && h % 2 == 0,
        "AVC444 pack needs even dimensions"
    );
    let (cw, ch) = (w / 2, h / 2);
    let chroma_off = w * 2 * h;
    let mut out = vec![0u8; stacked_nv12_len(w, h)];

    // Main luma (rows 0..H) = the image's luma plane.
    for row in 0..h {
        out[row * w..row * w + w].copy_from_slice(&y[row * y_stride..row * y_stride + w]);
    }
    // Aux luma (rows H..2H) tiling + aux/main chroma planes — one pass over chroma quadrants.
    for yc in 0..ch {
        for xc in 0..cw {
            let c = |p: &[u8], i: usize, j: usize| p[(2 * yc + j) * c_stride + (2 * xc + i)];
            let (cb00, cb01, cb10, cb11) = (c(cb, 0, 0), c(cb, 1, 0), c(cb, 0, 1), c(cb, 1, 1));
            let (cr00, cr01, cr10, cr11) = (c(cr, 0, 0), c(cr, 1, 0), c(cr, 0, 1), c(cr, 1, 1));
            // aux luma 2×2 tiling: [Cb01 | Cb10 ; Cb11 | Cr01]
            out[(h + yc) * w + xc] = cb01; // TL
            out[(h + yc) * w + (cw + xc)] = cb10; // TR
            out[(h + ch + yc) * w + xc] = cb11; // BL
            out[(h + ch + yc) * w + (cw + xc)] = cr01; // BR
            // main chroma (rows 0..H/2): U=Cb00, V=Cr00
            out[chroma_off + yc * w + 2 * xc] = cb00;
            out[chroma_off + yc * w + 2 * xc + 1] = cr00;
            // aux chroma (rows H/2..H): U=Cr10, V=Cr11
            out[chroma_off + (ch + yc) * w + 2 * xc] = cr10;
            out[chroma_off + (ch + yc) * w + 2 * xc + 1] = cr11;
        }
    }
    out
}

/// Inverse of [`pack_y444_to_stacked_nv12`]: reconstruct full Y/Cb/Cr (tight `w×h` planes)
/// from a tightly-packed stacked `W×2H` NV12 buffer. The CPU reference used by the viewer's
/// fallback path and the round-trip test.
pub fn unpack_stacked_nv12_to_y444(buf: &[u8], w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    assert!(
        w % 2 == 0 && h % 2 == 0,
        "AVC444 unpack needs even dimensions"
    );
    let (cw, ch) = (w / 2, h / 2);
    let chroma_off = w * 2 * h;
    let mut y = vec![0u8; w * h];
    let mut cb = vec![0u8; w * h];
    let mut cr = vec![0u8; w * h];
    for py in 0..h {
        for px in 0..w {
            y[py * w + px] = buf[py * w + px]; // main luma
            let (i, j, xc, yc) = (px & 1, py & 1, px >> 1, py >> 1);
            let cb_v = match (i, j) {
                (0, 0) => buf[chroma_off + yc * w + 2 * xc], // Cb00 (main U)
                (1, 0) => buf[(h + yc) * w + xc],            // Cb01 (aux TL)
                (0, 1) => buf[(h + yc) * w + (cw + xc)],     // Cb10 (aux TR)
                _ => buf[(h + ch + yc) * w + xc],            // Cb11 (aux BL)
            };
            let cr_v = match (i, j) {
                (0, 0) => buf[chroma_off + yc * w + 2 * xc + 1], // Cr00 (main V)
                (1, 0) => buf[(h + ch + yc) * w + (cw + xc)],    // Cr01 (aux BR)
                (0, 1) => buf[chroma_off + (ch + yc) * w + 2 * xc], // Cr10 (aux U)
                _ => buf[chroma_off + (ch + yc) * w + 2 * xc + 1], // Cr11 (aux V)
            };
            cb[py * w + px] = cb_v;
            cr[py * w + px] = cr_v;
        }
    }
    (y, cb, cr)
}

/// Reconstruct tightly-packed **RGBA** (`w×h`, stride `4·w`) directly from a decoded stacked
/// `W×2H` NV12 with arbitrary plane strides (as the decoder hands us). Combines the quadrant
/// gather with **BT.601 limited-range** YCbCr→RGB — must match the encoder's Y444 colorimetry.
/// This is the viewer's CPU reconstruction path (correctness baseline; the GL shader does the
/// same math on-GPU for the hot path).
pub fn unpack_stacked_nv12_to_rgba(
    luma: &[u8],
    luma_stride: usize,
    chroma: &[u8],
    chroma_stride: usize,
    w: usize,
    h: usize,
) -> Vec<u8> {
    assert!(
        w % 2 == 0 && h % 2 == 0,
        "AVC444 unpack needs even dimensions"
    );
    let ch = h / 2;
    let mut out = vec![0u8; w * h * 4];

    // Each output row is independent (it gathers from the shared luma/chroma planes and writes
    // only its own row), so split the rows across CPU cores. The single-threaded unpack of a
    // 1440p frame caps a client around ~27fps — too slow to reconstruct a 60fps 4:4:4 stream.
    // This is the exact inverse gather of `pack_y444_to_stacked_nv12` (byte-identical output, the
    // round-trip test guards it); `std::thread::scope` keeps it dependency-free.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, h.max(1));
    let rows_per = h.div_ceil(threads);
    std::thread::scope(|s| {
        let mut y0 = 0usize;
        for chunk in out.chunks_mut(rows_per * w * 4) {
            let nrows = chunk.len() / (w * 4);
            let base = y0;
            s.spawn(move || {
                for r in 0..nrows {
                    let py = base + r;
                    for px in 0..w {
                        let yv = luma[py * luma_stride + px]; // main luma (rows 0..h)
                        let (i, j, xc, yc) = (px & 1, py & 1, px >> 1, py >> 1);
                        let cb = match (i, j) {
                            (0, 0) => chroma[yc * chroma_stride + 2 * xc],
                            (1, 0) => luma[(h + yc) * luma_stride + xc],
                            (0, 1) => luma[(h + yc) * luma_stride + (w / 2 + xc)],
                            _ => luma[(h + ch + yc) * luma_stride + xc],
                        };
                        let cr = match (i, j) {
                            (0, 0) => chroma[yc * chroma_stride + 2 * xc + 1],
                            (1, 0) => luma[(h + ch + yc) * luma_stride + (w / 2 + xc)],
                            (0, 1) => chroma[(ch + yc) * chroma_stride + 2 * xc],
                            _ => chroma[(ch + yc) * chroma_stride + 2 * xc + 1],
                        };
                        let (rr, gg, bb) = ycbcr_to_rgb_bt601(yv, cb, cr);
                        let o = (r * w + px) * 4;
                        chunk[o] = rr;
                        chunk[o + 1] = gg;
                        chunk[o + 2] = bb;
                        chunk[o + 3] = 255;
                    }
                }
            });
            y0 += nrows;
        }
    });
    out
}

/// BT.601 limited ("studio") range YCbCr→RGB.
#[inline]
fn ycbcr_to_rgb_bt601(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let c = (y as f32 - 16.0) * 1.164_383;
    let d = cb as f32 - 128.0;
    let e = cr as f32 - 128.0;
    let clamp = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    (
        clamp(c + 1.596_027 * e),
        clamp(c - 0.391_762 * d - 0.812_968 * e),
        clamp(c + 2.017_232 * d),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic pseudo-random fill (no rand dep; Date/rand are unavailable anyway).
    fn fill(buf: &mut [u8], seed: u64) {
        let mut s = seed.wrapping_add(0x9E3779B97F4A7C15);
        for b in buf.iter_mut() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *b = (s & 0xFF) as u8;
        }
    }

    #[test]
    fn pack_unpack_roundtrip_is_lossless() {
        // A few even sizes incl. the 1440p/1080p targets (small enough to be fast).
        for &(w, h) in &[
            (4usize, 4usize),
            (8, 6),
            (64, 48),
            (1920, 1080),
            (2560, 1440),
        ] {
            let (mut y, mut cb, mut cr) = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
            fill(&mut y, 1);
            fill(&mut cb, 2);
            fill(&mut cr, 3);
            let packed = pack_y444_to_stacked_nv12(&y, &cb, &cr, w, h, w, w);
            assert_eq!(packed.len(), stacked_nv12_len(w, h));
            let (y2, cb2, cr2) = unpack_stacked_nv12_to_y444(&packed, w, h);
            assert!(y == y2, "luma mismatch at {w}x{h}");
            assert!(cb == cb2, "Cb mismatch at {w}x{h}");
            assert!(cr == cr2, "Cr mismatch at {w}x{h}");
        }
    }

    #[test]
    fn rgba_unpack_matches_gather_and_matrix() {
        // BT.601 limited-range endpoints.
        assert_eq!(ycbcr_to_rgb_bt601(16, 128, 128), (0, 0, 0)); // black
        assert_eq!(ycbcr_to_rgb_bt601(235, 128, 128), (255, 255, 255)); // white
        // The RGBA path must gather the same Cb/Cr as the Y444 reference, then apply the
        // matrix — verify on a random frame that RGBA == matrix(Y444-gather) pixel-wise.
        let (w, h) = (16usize, 12usize);
        let (mut y, mut cb, mut cr) = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
        fill(&mut y, 11);
        fill(&mut cb, 12);
        fill(&mut cr, 13);
        let packed = pack_y444_to_stacked_nv12(&y, &cb, &cr, w, h, w, w);
        let (yg, cbg, crg) = unpack_stacked_nv12_to_y444(&packed, w, h);
        // Split the tight stacked buffer into its luma (rows 0..2h) and chroma (rows 0..h) planes.
        let (luma, chroma) = packed.split_at(w * 2 * h);
        let rgba = unpack_stacked_nv12_to_rgba(luma, w, chroma, w, w, h);
        for p in 0..w * h {
            let (r, g, b) = ycbcr_to_rgb_bt601(yg[p], cbg[p], crg[p]);
            assert_eq!(
                (
                    rgba[p * 4],
                    rgba[p * 4 + 1],
                    rgba[p * 4 + 2],
                    rgba[p * 4 + 3]
                ),
                (r, g, b, 255)
            );
        }
    }

    #[test]
    fn pack_handles_input_stride_padding() {
        let (w, h) = (8usize, 4usize);
        let (ys, cs) = (w + 5, w + 3); // padded strides
        let (mut y, mut cb, mut cr) = (vec![0u8; ys * h], vec![0u8; cs * h], vec![0u8; cs * h]);
        fill(&mut y, 7);
        fill(&mut cb, 8);
        fill(&mut cr, 9);
        let packed = pack_y444_to_stacked_nv12(&y, &cb, &cr, w, h, ys, cs);
        let (y2, cb2, cr2) = unpack_stacked_nv12_to_y444(&packed, w, h);
        // Compare against the unpadded view of the source.
        for row in 0..h {
            assert_eq!(&y2[row * w..row * w + w], &y[row * ys..row * ys + w]);
            assert_eq!(&cb2[row * w..row * w + w], &cb[row * cs..row * cs + w]);
            assert_eq!(&cr2[row * w..row * w + w], &cr[row * cs..row * cs + w]);
        }
    }
}
