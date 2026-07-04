//! Standalone media plane (Phase-4 → Phase-5 bridge): ingest dmabuf from
//! clone-daemon, VA-API H.264 encode, and serve the access units to a viewer over
//! TCP as `[u32be len][Annex-B AU]` (the PoC-validated framing; the structured
//! `wire` viewer protocol + multi-monitor come next). This logic folds into the
//! control-server's port 1.
//!
//!   RMNG_SOCKET=/tmp/rmng-media.sock RMNG_VIDEO_PORT=9001 media-server

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use media::{Encoder, Listener};
use wire::socket::{Ack, DaemonMsg, ServerMsg};

type Viewer = Arc<Mutex<Option<TcpStream>>>;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    gstreamer::init()?;

    let sock_path = std::env::var("RMNG_SOCKET").unwrap_or_else(|_| "/tmp/rmng-media.sock".into());
    let video_port: u16 =
        std::env::var("RMNG_VIDEO_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(9001);

    let viewer: Viewer = Arc::new(Mutex::new(None));

    // Encoder → frame each AU to the connected viewer.
    let enc = {
        let viewer = viewer.clone();
        Encoder::new(wire::ChromaMode::Yuv420, move |au, _idr| {
            // Single-monitor harness: frame as monitor 0 ([u32 mid][u32 len][AnnexB]).
            let mut guard = viewer.lock().unwrap();
            if let Some(sock) = guard.as_mut() {
                let ok = sock
                    .write_all(&0u32.to_be_bytes())
                    .and_then(|_| sock.write_all(&(au.len() as u32).to_be_bytes()))
                    .and_then(|_| sock.write_all(&au));
                if ok.is_err() {
                    tracing::info!("viewer disconnected");
                    *guard = None;
                }
            }
        })?
    };

    // TCP accept loop (port 1): newest viewer wins.
    {
        let viewer = viewer.clone();
        let listener = TcpListener::bind(("0.0.0.0", video_port))?;
        tracing::info!("port 1 (video) listening on 0.0.0.0:{video_port}");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_nodelay(true);
                tracing::info!("viewer connected: {:?}", stream.peer_addr());
                *viewer.lock().unwrap() = Some(stream);
            }
        });
    }

    // Clone-daemon ingest loop.
    let listener = Listener::bind(&sock_path)?;
    tracing::info!("media socket listening on {sock_path}");
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
            Ok(_) => {}
            Err(e) => {
                tracing::info!("clone socket closed: {e}");
                break;
            }
        }
    }
    Ok(())
}
