//! Standalone Phase-4 harness: listen on a media socket, receive dmabuf frames
//! from clone-daemon, encode them to H.264, and report encode fps + the first IDR
//! size. Proves the cross-process dmabuf → VA-API H.264 path end-to-end.
//!
//!   RMNG_SOCKET=/tmp/rmng-media.sock encode-test

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::Result;
use media::{Encoder, Listener};
use wire::socket::{Ack, DaemonMsg, ServerMsg};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    gstreamer::init()?;

    let path = std::env::var("RMNG_SOCKET").unwrap_or_else(|_| "/tmp/rmng-media.sock".into());

    let encoded = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let first_idr = Arc::new(AtomicBool::new(true));
    let enc = {
        let (e, b, fi) = (encoded.clone(), bytes.clone(), first_idr.clone());
        Encoder::new(wire::ChromaMode::Yuv420, move |au, idr| {
            e.fetch_add(1, Ordering::Relaxed);
            b.fetch_add(au.len() as u64, Ordering::Relaxed);
            if idr && fi.swap(false, Ordering::Relaxed) {
                tracing::info!("first IDR: {} bytes", au.len());
            }
        })?
    };

    // fps reporter.
    {
        let (e, b) = (encoded.clone(), bytes.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let n = e.swap(0, Ordering::Relaxed);
            let kb = b.swap(0, Ordering::Relaxed) / 1024;
            tracing::info!("encode fps: {n} ({kb} KiB/s)");
        });
    }

    let listener = Listener::bind(&path)?;
    tracing::info!("media encode-test listening on {path}");
    let conn = listener.accept()?;
    tracing::info!("clone-daemon connected");

    loop {
        match conn.recv() {
            Ok((DaemonMsg::Frame(f), fds)) => {
                if let Some(fd) = fds.into_iter().next() {
                    if let Err(e) = enc.push(fd, f.fourcc, f.modifier, f.width, f.height, &f.planes) {
                        tracing::warn!("encode push failed: {e}");
                    }
                }
                let _ = conn.send(&ServerMsg::Ack(Ack { monitor_id: f.monitor_id, seq: f.seq }));
            }
            Ok(_) => {} // cursor/clipboard — ignored in this harness
            Err(e) => {
                tracing::info!("socket closed: {e}");
                break;
            }
        }
    }
    Ok(())
}
