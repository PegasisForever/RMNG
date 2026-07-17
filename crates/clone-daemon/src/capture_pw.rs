//! Raw-PipeWire screen capture for Mutter ScreenCast nodes.
//!
//! Unlike the GStreamer path in `capture.rs`, this connects to the PipeWire node
//! directly (crate `pipewire`/`libspa`) so it can read the **`SPA_META_Cursor`**
//! buffer metadata that `pipewiresrc` doesn't surface: the cursor position and,
//! when it changes, the cursor shape bitmap. That's the whole reason this module
//! exists — the client draws the cursor locally instead of having Mutter
//! composite it into the frame, so the stream must be created in cursor-mode
//! METADATA (see `mutter::CURSOR_MODE_METADATA`).
//!
//! Frames are negotiated as **DMABUF** with the explicit DRM modifier the W6800
//! offers (so the encoder can import them zero-copy), falling back to plain
//! shared-memory `MemPtr`/`MemFd` buffers if the node only offers those. The
//! cursor metadata is read identically regardless of the buffer type.
//!
//! The PipeWire mainloop is blocking and its objects are `!Send`, so the caller
//! must build the `MainLoop` and run this on its own dedicated thread.

use std::os::fd::{FromRawFd, OwnedFd};

use anyhow::{Context as _, Result};
use pipewire as pw;
use pw::spa;
use spa::param::video::{VideoFormat, VideoInfoRaw};
use spa::pod::serialize::PodSerializer;
use spa::pod::{Object, Pod, Property, PropertyFlags, Value};
use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id, Rectangle};

/// The W6800-validated DRM format: AR24 (ARGB8888) with AMD's tiled modifier.
/// SPA `VideoFormat::BGRA` maps to DRM fourcc AR24 / ARGB8888 (little-endian).
/// fourcc('A','R','2','4') = 'A' | 'R'<<8 | '2'<<16 | '4'<<24 = 0x34325241.
pub const DRM_FOURCC_AR24: u32 = 0x3432_5241;
pub const DRM_MODIFIER_AMD_TILED: u64 = 0x0200_0000_2080_1b03;
pub const DRM_MODIFIER_LINEAR: u64 = 0; // == DRM_FORMAT_MOD_LINEAR

/// One captured dmabuf (or shm) frame: everything needed to import it elsewhere.
pub struct PwFrame {
    /// DRM fourcc, e.g. AR24 (ARGB8888) = 0x34325241.
    pub fourcc: u32,
    /// DRM modifier (0 / LINEAR for shm buffers).
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    /// (offset, stride) per plane.
    pub planes: Vec<(u32, u32)>,
    /// dup'd dmabuf fds in plane order. Empty for shm/MemPtr frames (no fd).
    pub fds: Vec<OwnedFd>,
}

/// A cursor update. `x`/`y` is the latest screen position; `shape` is `Some`
/// only on the frame where the cursor bitmap changed.
pub struct PwCursor {
    pub x: i32,
    pub y: i32,
    /// `Some` only when the shape changes: (width, height, hotspot_x, hotspot_y,
    /// pixel bytes). The bytes are the raw bitmap as delivered by SPA — for
    /// Mutter that is BGRA/ARGB8888 (4 bytes/pixel, premultiplied alpha),
    /// `width * height * 4` long after stride is removed.
    pub shape: Option<(u32, u32, u32, u32, Vec<u8>)>,
    /// Cursor visibility, known only on frames carrying a bitmap change:
    /// `Some(true)` = the cursor was hidden (empty bitmap — native Wayland hides —
    /// or an all-zero sprite — how Xwayland apps hide); `Some(false)` = a new
    /// visible sprite (`shape` is `Some`); `None` = position-only, no change.
    pub hidden: Option<bool>,
}

/// A sprite whose bytes are all zero draws nothing: X11/Xwayland apps "hide"
/// the cursor by setting such a bitmap (alpha 0 everywhere — and premultiplied
/// alpha zeroes the color channels too).
fn is_invisible_bitmap(pixels: &[u8]) -> bool {
    pixels.iter().all(|&b| b == 0)
}

/// Per-stream user data carried through the PipeWire listener callbacks.
struct UserData {
    /// Negotiated raw-video format (size + chosen modifier), set in param_changed.
    format: Option<VideoInfoRaw>,
    on_frame: Box<dyn FnMut(PwFrame)>,
    on_cursor: Box<dyn FnMut(PwCursor)>,
}

/// `#[repr(transparent)]` view over `spa_meta_cursor` so we can fetch it with the
/// pipewire crate's typed `Buffer::find_meta` (which calls
/// `spa_buffer_find_meta_data(buffer, META_TYPE, size_of::<T>())` — the on-buffer
/// meta is larger, holding the bitmap inline, but the size check is `>=`).
#[repr(transparent)]
struct CursorMeta(spa::sys::spa_meta_cursor);

impl spa::buffer::meta::Metadata for CursorMeta {
    const META_TYPE: u32 = spa::sys::SPA_META_Cursor;
}

/// Connect to `node_id`, run the PipeWire mainloop **forever** (blocking — the
/// caller runs this on its own thread). `on_frame` fires per dmabuf/shm frame;
/// `on_cursor` fires when the cursor position or shape changes.
///
/// `pw::init()` must have been called once before this (the test bin / caller
/// does it). The `MainLoop` is created here so all PipeWire objects (which are
/// `!Send`) stay on this thread.
pub fn run<F, G>(node_id: u32, on_frame: F, on_cursor: G) -> Result<()>
where
    F: FnMut(PwFrame) + 'static,
    G: FnMut(PwCursor) + 'static,
{
    // pipewire 0.10's owning wrappers are the `*Rc` types (the bare MainLoop/
    // Context/Stream are borrowed views). MainLoopRc::new also calls pw::init().
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("creating pipewire main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None).context("creating pipewire context")?;
    let core = context.connect_rc(None).context("connecting to pipewire daemon")?;

    let stream = pw::stream::StreamRc::new(
        core,
        &format!("clone-daemon-capture-{node_id}"),
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .context("creating pipewire stream")?;

    let data = UserData {
        format: None,
        on_frame: Box::new(on_frame),
        on_cursor: Box::new(on_cursor),
    };

    // Keep the listener alive for the loop's lifetime.
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, _, old, new| {
            tracing::debug!("pw stream state: {old:?} -> {new:?}");
        })
        .add_buffer(|_, _, pw_buf| {
            // Log the metas Mutter allocated on a buffer once, at debug level — a
            // quick way to confirm SPA_META_Cursor (type 5) made it into the pool.
            if tracing::enabled!(tracing::Level::DEBUG) {
                // SAFETY: pw_buf is a valid pw_buffer for this callback's duration.
                unsafe {
                    let spa_buf = (*pw_buf).buffer;
                    if spa_buf.is_null() {
                        return;
                    }
                    let n = (*spa_buf).n_metas;
                    let metas = (*spa_buf).metas;
                    let types: Vec<(u32, u32)> = (0..n as isize)
                        .map(|i| {
                            let m = &*metas.offset(i);
                            (m.type_, m.size)
                        })
                        .collect();
                    tracing::debug!("buffer added: n_metas={n} metas(type,size)={types:?}");
                }
            }
        })
        .param_changed(move |stream, data, id, param| {
            on_param_changed(stream, data, id, param);
        })
        .process(move |stream, data| {
            on_process(stream, data);
        })
        .register()
        .context("registering pipewire stream listener")?;

    // EnumFormat advertising DMABUF (AR24 + AMD-tiled/LINEAR modifiers). The
    // presence of the modifier property is what makes Mutter offer DMABUF.
    let format_pod = build_enum_format_pod()?;
    let pod = Pod::from_bytes(&format_pod).context("EnumFormat pod is not a valid pod")?;

    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            // No MAP_BUFFERS: we want the raw dmabuf fd, not a CPU mapping.
            pw::stream::StreamFlags::AUTOCONNECT,
            &mut [pod],
        )
        .context("connecting pipewire stream to node")?;

    tracing::info!(node_id, "raw-pipewire capture connected; running mainloop");
    mainloop.run();
    Ok(())
}

/// Build the `EnumFormat` POD as an explicit `Value::Object` so we control the
/// modifier property's flags (MANDATORY | DONT_FIXATE) — the `property!` macro
/// doesn't expose per-property flags. Advertising a modifier Choice is what
/// makes Mutter negotiate DMABUF; without it the node hands back shm buffers.
fn build_enum_format_pod() -> Result<Vec<u8>> {
    let obj = Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property {
                key: spa::sys::SPA_FORMAT_mediaType,
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(spa::sys::SPA_MEDIA_TYPE_video)),
            },
            Property {
                key: spa::sys::SPA_FORMAT_mediaSubtype,
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(spa::sys::SPA_MEDIA_SUBTYPE_raw)),
            },
            // BGRA == DRM ARGB8888 / AR24.
            Property {
                key: spa::sys::SPA_FORMAT_VIDEO_format,
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(VideoFormat::BGRA.as_raw())),
            },
            // The modifier Choice. MANDATORY|DONT_FIXATE asks the node to either
            // honour a listed modifier or renegotiate — its presence triggers
            // the DMABUF path. List the AMD-tiled modifier first, then LINEAR.
            Property {
                key: spa::sys::SPA_FORMAT_VIDEO_modifier,
                flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
                value: Value::Choice(spa::pod::ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: DRM_MODIFIER_AMD_TILED as i64,
                        alternatives: vec![
                            DRM_MODIFIER_AMD_TILED as i64,
                            DRM_MODIFIER_LINEAR as i64,
                        ],
                    },
                ))),
            },
            Property {
                key: spa::sys::SPA_FORMAT_VIDEO_size,
                flags: PropertyFlags::empty(),
                value: Value::Choice(spa::pod::ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle { width: 1920, height: 1080 },
                        min: Rectangle { width: 1, height: 1 },
                        max: Rectangle { width: 16384, height: 16384 },
                    },
                ))),
            },
            Property {
                key: spa::sys::SPA_FORMAT_VIDEO_framerate,
                flags: PropertyFlags::empty(),
                value: Value::Choice(spa::pod::ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: spa::utils::Fraction { num: 60, denom: 1 },
                        min: spa::utils::Fraction { num: 0, denom: 1 },
                        max: spa::utils::Fraction { num: 1000, denom: 1 },
                    },
                ))),
            },
        ],
    };

    let (cursor, _) =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .context("serializing EnumFormat pod")?;
    Ok(cursor.into_inner())
}

/// On Format negotiation: parse the chosen format, then answer with our Buffers
/// (request DMABUF data type) and Meta (request SPA_META_Cursor) params.
fn on_param_changed(stream: &pw::stream::Stream, data: &mut UserData, id: u32, param: Option<&Pod>) {
    let Some(param) = param else { return };
    if id != spa::param::ParamType::Format.as_raw() {
        return;
    }
    let (media_type, media_subtype) = match spa::param::format_utils::parse_format(param) {
        Ok(v) => v,
        Err(_) => return,
    };
    if media_type != spa::param::format::MediaType::Video
        || media_subtype != spa::param::format::MediaSubtype::Raw
    {
        return;
    }
    let mut info = VideoInfoRaw::default();
    if info.parse(param).is_err() {
        tracing::warn!("failed to parse negotiated video format");
        return;
    }
    let has_modifier = info
        .flags()
        .contains(spa::param::video::VideoFlags::MODIFIER);
    tracing::info!(
        "pw format negotiated: {:?} {}x{} modifier={:#018x} (dmabuf={})",
        info.format(),
        info.size().width,
        info.size().height,
        info.modifier(),
        has_modifier,
    );
    data.format = Some(info);

    // Build the response params: Buffers (advertise DMABUF) + Meta (cursor).
    let buffers_pod = match build_buffers_pod() {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("building Buffers pod: {e:#}");
            return;
        }
    };
    let meta_pod = match build_meta_cursor_pod() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("building Meta pod: {e:#}");
            return;
        }
    };
    let (Some(buffers), Some(meta)) =
        (Pod::from_bytes(&buffers_pod), Pod::from_bytes(&meta_pod))
    else {
        tracing::error!("response pods are not valid pods");
        return;
    };
    match stream.update_params(&mut [buffers, meta]) {
        Ok(()) => tracing::debug!("update_params: Buffers + Meta(cursor) submitted"),
        Err(e) => tracing::error!("update_params failed: {e}"),
    }
}

/// Buffers param: allow DMABUF, MemFd and MemPtr data types (let the node pick).
fn build_buffers_pod() -> Result<Vec<u8>> {
    let data_types = (1 << spa::sys::SPA_DATA_DmaBuf)
        | (1 << spa::sys::SPA_DATA_MemFd)
        | (1 << spa::sys::SPA_DATA_MemPtr);
    let obj = Object {
        type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            Property {
                key: spa::sys::SPA_PARAM_BUFFERS_buffers,
                flags: PropertyFlags::empty(),
                value: Value::Choice(spa::pod::ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range { default: 4, min: 2, max: 16 },
                ))),
            },
            Property {
                key: spa::sys::SPA_PARAM_BUFFERS_blocks,
                flags: PropertyFlags::empty(),
                value: Value::Int(1),
            },
            Property {
                key: spa::sys::SPA_PARAM_BUFFERS_dataType,
                flags: PropertyFlags::MANDATORY,
                value: Value::Choice(spa::pod::ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags { default: data_types as i32, flags: vec![] },
                ))),
            },
        ],
    };
    let (cursor, _) =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .context("serializing Buffers pod")?;
    Ok(cursor.into_inner())
}

/// Size of the cursor meta needed to hold a `w`×`h` BGRA bitmap inline (the C
/// `CURSOR_META_SIZE(w,h)` macro: cursor header + bitmap header + pixels).
fn cursor_meta_size(w: u32, h: u32) -> i32 {
    (std::mem::size_of::<spa::sys::spa_meta_cursor>()
        + std::mem::size_of::<spa::sys::spa_meta_bitmap>()
        + (w * h * 4) as usize) as i32
}

/// Meta param: request SPA_META_Cursor. The producer (Mutter) advertises the
/// cursor meta in its own param list with a **fixed** `SPA_PARAM_META_size =
/// CURSOR_META_SIZE(384, 384)`; the buffer pool is the intersection of producer
/// and consumer params, so our advertised size range MUST cover Mutter's 384×384
/// value or the cursor meta is dropped from the pool. We advertise a range up to
/// 384×384 (matching Mutter) so the negotiation keeps the meta.
fn build_meta_cursor_pod() -> Result<Vec<u8>> {
    let obj = Object {
        type_: spa::utils::SpaTypes::ObjectParamMeta.as_raw(),
        id: spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            Property {
                key: spa::sys::SPA_PARAM_META_type,
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(spa::sys::SPA_META_Cursor)),
            },
            Property {
                key: spa::sys::SPA_PARAM_META_size,
                flags: PropertyFlags::empty(),
                value: Value::Choice(spa::pod::ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: cursor_meta_size(384, 384),
                        min: cursor_meta_size(1, 1),
                        max: cursor_meta_size(384, 384),
                    },
                ))),
            },
        ],
    };
    let (cursor, _) =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .context("serializing Meta pod")?;
    Ok(cursor.into_inner())
}

/// Per-buffer processing: emit the dmabuf/shm frame and any cursor update.
/// We process every dequeued buffer for the cursor meta (a shape change could
/// land on a buffer that's superseded for frame purposes), but only ship the
/// freshest buffer's frame.
fn on_process(stream: &pw::stream::Stream, data: &mut UserData) {
    let Some(info) = data.format else { return };
    while let Some(mut buffer) = stream.dequeue_buffer() {
        // Cursor metadata first (independent of the frame data type / size).
        if let Some(cursor) = read_cursor(&buffer) {
            (data.on_cursor)(cursor);
        }
        // Frame (dmabuf fds or shm + plane layout). Skipped when there's no pixel
        // data this cycle (e.g. a cursor-only update has an empty chunk) — in
        // cursor-mode METADATA, pointer motion alone produces no frame damage.
        if let Some(frame) = read_frame(&mut buffer, &info) {
            (data.on_frame)(frame);
        }
    }
}

/// Read `SPA_META_Cursor` from the buffer. Returns `None` if the cursor meta is
/// absent or marks "no new cursor data" (id == 0). `shape` is `Some` only when a
/// new bitmap is present (bitmap_offset != 0).
fn read_cursor(buffer: &pw::buffer::Buffer<'_>) -> Option<PwCursor> {
    // SAFETY: `find_meta::<CursorMeta>()` (CursorMeta is repr(transparent) over
    // spa_meta_cursor) returns a pointer into the buffer's metadata region,
    // valid for the lifetime of this borrow. We only read it.
    let meta = buffer.find_meta::<CursorMeta>()?;
    let c = &meta.0;
    // id 0 means "no new cursor data" for this buffer.
    if c.id == 0 {
        return None;
    }

    let x = c.position.x;
    let y = c.position.y;

    // bitmap_offset == 0 → no new bitmap; just a position update.
    if c.bitmap_offset == 0 {
        return Some(PwCursor { x, y, shape: None, hidden: None });
    }

    // SAFETY: per the SPA contract, when bitmap_offset >= size_of::<spa_meta_cursor>()
    // there is a spa_meta_bitmap at `(cursor_ptr + bitmap_offset)`, and its pixels
    // at `(bitmap_ptr + bitmap.offset)`, all inside the cursor meta region the
    // producer allocated (we negotiated CURSOR_META_SIZE(384, 384)).
    let (shape, hidden) = unsafe {
        let cursor_ptr = c as *const spa::sys::spa_meta_cursor as *const u8;
        let bitmap_ptr =
            cursor_ptr.add(c.bitmap_offset as usize) as *const spa::sys::spa_meta_bitmap;
        let bitmap = &*bitmap_ptr;
        let bw = bitmap.size.width;
        let bh = bitmap.size.height;
        if bitmap.offset == 0 || bw == 0 || bh == 0 {
            // Empty bitmap: the cursor was HIDDEN (how native Wayland grabs hide it).
            (None, Some(true))
        } else if bw > 384 || bh > 384 {
            // Malformed/oversized: clamp to the negotiated 384×384 so a bad bitmap
            // can't drive an OOB read; says nothing about visibility.
            (None, None)
        } else {
            let stride = bitmap.stride.max(0) as usize;
            let row_bytes = bw as usize * 4; // BGRA/ARGB8888, 4 bpp
            let pixels_ptr = (bitmap_ptr as *const u8).add(bitmap.offset as usize);
            // Copy out row by row, dropping any stride padding so the caller gets
            // a tightly-packed width*height*4 buffer.
            let mut pixels = Vec::with_capacity(row_bytes * bh as usize);
            for row in 0..bh as usize {
                let row_start = pixels_ptr.add(row * stride);
                let row_slice = std::slice::from_raw_parts(row_start, row_bytes);
                pixels.extend_from_slice(row_slice);
            }
            if is_invisible_bitmap(&pixels) {
                // All-zero sprite: an Xwayland app's cursor hide.
                (None, Some(true))
            } else {
                (Some((bw, bh, c.hotspot.x as u32, c.hotspot.y as u32, pixels)), Some(false))
            }
        }
    };

    Some(PwCursor { x, y, shape, hidden })
}

/// Read the frame's plane layout + dmabuf fds (or empty fds for shm buffers).
fn read_frame(buffer: &mut pw::buffer::Buffer<'_>, info: &VideoInfoRaw) -> Option<PwFrame> {
    let width = info.size().width;
    let height = info.size().height;
    if width == 0 || height == 0 {
        return None;
    }

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return None;
    }

    let mut planes: Vec<(u32, u32)> = Vec::with_capacity(datas.len());
    let mut fds: Vec<OwnedFd> = Vec::new();
    let mut is_dmabuf = false;
    let mut first_size = 0u32;

    for (i, d) in datas.iter_mut().enumerate() {
        let chunk = d.chunk();
        let offset = chunk.offset();
        let stride = chunk.stride().max(0) as u32;
        if i == 0 {
            // A chunk size of 0 on plane 0 means there's no pixel data this cycle.
            first_size = chunk.size();
        }
        planes.push((offset, stride));

        let raw = d.as_raw();
        if raw.type_ == spa::sys::SPA_DATA_DmaBuf {
            is_dmabuf = true;
            if raw.fd >= 0 {
                // dup() the borrowed dmabuf fd — Mutter recycles the original buffer.
                match nix::unistd::dup(raw.fd as std::os::fd::RawFd) {
                    Ok(dup) => {
                        // SAFETY: `dup` is a fresh fd we exclusively own.
                        fds.push(unsafe { OwnedFd::from_raw_fd(dup) });
                    }
                    Err(e) => {
                        tracing::warn!("dup dmabuf fd failed: {e}");
                        return None;
                    }
                }
            }
        }
    }

    // Skip an all-empty buffer (e.g. a cursor-only update with no frame damage).
    if first_size == 0 {
        return None;
    }

    let (fourcc, modifier) = if is_dmabuf {
        (DRM_FOURCC_AR24, info.modifier())
    } else {
        // shm fallback: tightly-packed AR24 in CPU memory, no DRM modifier.
        (DRM_FOURCC_AR24, DRM_MODIFIER_LINEAR)
    };

    Some(PwFrame {
        fourcc,
        modifier,
        width,
        height,
        planes,
        fds,
    })
}

#[cfg(test)]
mod tests {
    use super::is_invisible_bitmap;

    #[test]
    fn invisible_bitmap_detection() {
        // Xwayland-style hide: a real-size sprite whose bytes are all zero.
        assert!(is_invisible_bitmap(&[0u8; 24 * 24 * 4]));
        // Any non-zero byte (one visible premultiplied pixel) makes it a real sprite.
        let mut px = vec![0u8; 24 * 24 * 4];
        px[3] = 0xff;
        assert!(!is_invisible_bitmap(&px));
        // Degenerate: an empty pixel buffer draws nothing.
        assert!(is_invisible_bitmap(&[]));
    }
}
