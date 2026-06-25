//! Standalone test for the raw-PipeWire capture (`capture_pw`).
//!
//! Sets up a Mutter ScreenCast session in **cursor-mode METADATA** (so the cursor
//! is delivered as `SPA_META_Cursor`, not composited into the frame), nudges the
//! pointer so Mutter emits damage + cursor metadata, and runs `capture_pw::run`
//! on a dedicated std::thread (the PipeWire mainloop is blocking and `!Send`).
//!
//! Logs once/second the frame count (fps) + the first frame's
//! fourcc/modifier/size/planes, and every cursor update as `cursor x,y shape=…`.

// The capture module + its sibling deps live in the clone-daemon binary crate, so
// this test bin re-declares the modules it needs as a path-included sub-tree.
#[path = "../capture_pw.rs"]
mod capture_pw;
// This diagnostic bin exercises a subset of `mutter` (METADATA cursor mode only);
// `CURSOR_MODE_EMBEDDED`, `Session.conn`, and `VirtualMonitor.monitor_id` are all live
// in the production daemon, just unused here.
#[path = "../mutter.rs"]
#[allow(dead_code)]
mod mutter;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Some deps (and the production daemon) init gstreamer; do it here too so the
    // process environment matches the real daemon.
    gstreamer::init()?;
    pipewire::init();

    tracing::info!("capture-pw-test: setting up Mutter session (cursor-mode METADATA)");
    let session = mutter::setup_with_cursor_mode(&[(1920, 1080)], mutter::CURSOR_MODE_METADATA)
        .await?;
    let mon = session
        .monitors
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no virtual monitor"))?;
    tracing::info!(
        node_id = mon.node_id,
        width = mon.width,
        height = mon.height,
        "virtual monitor ready"
    );

    // Nudge the cursor so Mutter generates damage (frames) + cursor metadata.
    nudge_cursor(&session);

    // Counters shared with the capture thread.
    let frame_count = Arc::new(AtomicU64::new(0));
    let first_logged = Arc::new(AtomicBool::new(false));
    let cursor_count = Arc::new(AtomicU64::new(0));

    // Per-second fps reporter.
    {
        let fc = frame_count.clone();
        let cc = cursor_count.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                tracing::info!(
                    "fps={} cursor_updates/s={}",
                    fc.swap(0, Ordering::Relaxed),
                    cc.swap(0, Ordering::Relaxed),
                );
            }
        });
    }

    // The PipeWire mainloop is blocking and !Send — run it on a dedicated std
    // thread, creating the MainLoop inside (done by capture_pw::run).
    let node_id = mon.node_id;
    let (fc, fl, cc) = (frame_count.clone(), first_logged.clone(), cursor_count.clone());
    let handle = std::thread::Builder::new()
        .name("capture-pw".into())
        .spawn(move || {
            let on_frame = move |frame: capture_pw::PwFrame| {
                fc.fetch_add(1, Ordering::Relaxed);
                if !fl.swap(true, Ordering::Relaxed) {
                    tracing::info!(
                        "first frame: fourcc={:#010x} modifier={:#018x} {}x{} planes={:?} fds={}",
                        frame.fourcc,
                        frame.modifier,
                        frame.width,
                        frame.height,
                        frame.planes,
                        frame.fds.len(),
                    );
                }
            };
            let on_cursor = move |cursor: capture_pw::PwCursor| {
                cc.fetch_add(1, Ordering::Relaxed);
                match &cursor.shape {
                    Some((w, h, hx, hy, bytes)) => tracing::info!(
                        "cursor {},{} shape={}x{} hotspot={},{} ({} bytes)",
                        cursor.x,
                        cursor.y,
                        w,
                        h,
                        hx,
                        hy,
                        bytes.len(),
                    ),
                    None => tracing::info!("cursor {},{} shape=None", cursor.x, cursor.y),
                }
            };
            if let Err(e) = capture_pw::run(node_id, on_frame, on_cursor) {
                tracing::error!("capture_pw::run failed: {e:#}");
            }
        })?;

    tracing::info!("capturing on PipeWire node {node_id} (Ctrl-C to stop) …");

    // Keep `session` alive (its zbus connection owns the Mutter session) and block
    // forever; the capture thread does the real work.
    let _session = session;
    let _ = handle; // detach: we run until killed (timeout in the test harness)
    futures::future::pending::<()>().await;
    Ok(())
}

/// Oscillate the pointer so the damage-driven capture emits frames and Mutter
/// emits cursor metadata (copied from `main.rs::nudge_cursor`).
fn nudge_cursor(session: &mutter::Session) {
    for m in &session.monitors {
        let rd = session.rd.clone();
        let stream = m.stream_path.clone();
        let (w, h) = (m.width as f64, m.height as f64);
        tokio::spawn(async move {
            let mut t = 0u32;
            loop {
                t = t.wrapping_add(1);
                // Bigger sweep than main.rs so the cursor visibly traverses the
                // monitor → Mutter reliably re-asserts the cursor on this output.
                let x = w / 2.0 + if t % 2 == 0 { 200.0 } else { -200.0 };
                let y = h / 2.0 + if t % 4 < 2 { 100.0 } else { -100.0 };
                let _ = rd.notify_pointer_motion_absolute(&stream, x, y).await;
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
        });
    }
}
