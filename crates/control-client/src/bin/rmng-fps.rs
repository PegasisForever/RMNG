//! `rmng-fps`: frame-rate test tool for the browser viewer's WebSocket. It joins
//! `ws://<host>/api/viewer?monitor=<n>` like one browser viewer window, reads each video
//! message's header (`wire::browser::VideoHeader`) without decoding, and prints once a second
//! how many frames, keyframes and megabits arrived.
//!
//! ```sh
//! cargo run -p control-client --bin rmng-fps -- <host>[:port] [monitor]
//! ```
//!
//! The port defaults to 9000 and the monitor to 0. It shows the selected clone, as a viewer
//! window does. Stop it with Ctrl+C.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio_tungstenite::tungstenite::Message;
use wire::browser::{KIND_VIDEO, VIDEO_HEADER_LEN};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(host) = args.next() else {
        bail!("usage: rmng-fps <host>[:port] [monitor]");
    };
    let host = if host.contains(':') {
        host
    } else {
        format!("{host}:{}", wire::PORT_WEB)
    };
    let monitor: u32 = args
        .next()
        .map_or(Ok(0), |m| m.parse())
        .context("monitor")?;
    let url = format!("ws://{host}/api/viewer?monitor={monitor}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("connect {url}"))?;
    eprintln!("connected to {url}");

    let (mut frames, mut keys, mut bytes) = (0u32, 0u32, 0usize);
    let mut since = Instant::now();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(1), ws.next()).await;
        match next {
            // Reading also answers the server's pings, which keep the socket open.
            Ok(Some(msg)) => {
                if let Message::Binary(b) = msg? {
                    if b.len() >= VIDEO_HEADER_LEN && b[0] == KIND_VIDEO {
                        frames += 1;
                        keys += (b[1] & 1) as u32;
                        bytes += b.len();
                    }
                }
            }
            Ok(None) => bail!("the server closed the socket"),
            Err(_) => {} // no message this second: still print the zero line
        }
        let dt = since.elapsed();
        if dt >= Duration::from_secs(1) {
            let s = dt.as_secs_f64();
            println!(
                "monitor {monitor}: {:5.1} fps  {keys} key  {:6.2} Mbit/s",
                frames as f64 / s,
                bytes as f64 * 8.0 / s / 1e6
            );
            (frames, keys, bytes) = (0, 0, 0);
            since = Instant::now();
        }
    }
}
