//! `rmng-fps`: frame-rate test tool for the browser viewer's WebSocket. It joins
//! `ws://<host>/api/viewer?monitor=<n>` like one browser viewer window, reads each video
//! message's header (`wire::browser::VideoHeader`) without decoding, and prints once a second
//! how many frames, keyframes and megabits arrived, and the delay.
//!
//! ```sh
//! cargo run -p control-client --bin rmng-fps -- <host>[:port] [monitor] [seconds]
//! ```
//!
//! The port defaults to 9000 and the monitor to 0. It shows the selected clone, as a viewer
//! window does. With `seconds` it stops after that time and prints a summary line; without,
//! stop it with Ctrl+C. When the server closes the socket, it connects again after 1 s, and
//! counts it.
//!
//! The delay is the time from an `echo` to its answer (every 200 ms). The server sends the
//! answer ahead of the video in its queue, so the delay is the round trip plus the wait in the
//! kernel send buffer and in any proxy on the way. The video's own delay is in the server's
//! log: `rmng-fps` acknowledges frames (`ack`), and the server logs the round trip and how far
//! behind the window is. It does not decode, so it acknowledges a frame when it arrives; a
//! viewer window acknowledges a frame when its decoder has output it.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use wire::browser::{FromBrowser, KIND_VIDEO, ToBrowser, VIDEO_HEADER_LEN};

const ECHO_EVERY: Duration = Duration::from_millis(200);
/// Acknowledge the newest frame this often when it changed, and every `ACK_HEARTBEAT` when it
/// did not (`ACK_MS` and `ACK_HEARTBEAT_MS` in `viewer/worker.ts`).
const ACK_EVERY: Duration = Duration::from_millis(50);
const ACK_HEARTBEAT: Duration = Duration::from_secs(1);
/// The wait before it connects again. (A viewer window tries at once, then after 400 ms
/// doubling up to 1 s; the bench needs no reconnect to be fast.)
const RECONNECT: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(host) = args.next() else {
        bail!("usage: rmng-fps <host>[:port] [monitor] [seconds]");
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
    let seconds: Option<u64> = args
        .next()
        .map(|s| s.parse())
        .transpose()
        .context("seconds")?;
    let url = format!("ws://{host}/api/viewer?monitor={monitor}");

    let start = Instant::now();
    let mut echo = tokio::time::interval(ECHO_EVERY);
    let mut ack = tokio::time::interval(ACK_EVERY);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // the first tick is at once
    let (mut frames, mut keys, mut bytes, mut delays) = (0u32, 0u32, 0usize, Vec::<f64>::new());
    let mut run = Run::default();
    let mut since = Instant::now();
    loop {
        let ws = match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => ws,
            Err(e) if run.reconnects == 0 => {
                return Err(e).with_context(|| format!("connect {url}"));
            }
            Err(e) => {
                eprintln!("connect {url}: {e}");
                tokio::time::sleep(RECONNECT).await;
                continue;
            }
        };
        eprintln!("connected to {url}");
        let (mut sink, mut stream) = ws.split();
        // The newest frame and when it arrived; the last acknowledged one and when.
        let mut newest: Option<(u32, Instant)> = None;
        let mut acked: Option<(u32, Instant)> = None;
        loop {
            tokio::select! {
                msg = stream.next() => match msg {
                    // Reading also answers the server's pings, which keep the socket open.
                    Some(Ok(Message::Binary(b))) => {
                        if b.len() >= VIDEO_HEADER_LEN && b[0] == KIND_VIDEO {
                            frames += 1;
                            keys += (b[1] & 1) as u32;
                            bytes += b.len();
                            let seq = u32::from_be_bytes(b[6..10].try_into().unwrap());
                            newest = Some((seq, Instant::now()));
                        }
                    }
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(ToBrowser::Echo { id }) = serde_json::from_str(&t) {
                            // The id is the send time in ms since the start.
                            delays.push(start.elapsed().as_millis() as f64 - id as f64);
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                },
                _ = echo.tick() => {
                    let id = start.elapsed().as_millis() as u32;
                    let msg = serde_json::to_string(&FromBrowser::Echo { id })?;
                    // A failed send closes the socket, and the read above sees that.
                    let _ = sink.send(Message::Text(msg)).await;
                }
                _ = ack.tick() => {
                    // Before the first frame the heartbeat names frame 0, as a window does.
                    let (seq, held_ms) =
                        newest.map_or((0, 0), |(s, at)| (s, at.elapsed().as_millis() as u32));
                    if acked.is_some_and(|(s, t)| s == seq && t.elapsed() < ACK_HEARTBEAT) {
                        continue;
                    }
                    // No clipboard comes here, so `clip` stays 0.
                    let msg = serde_json::to_string(&FromBrowser::Ack {
                        seq,
                        held_ms,
                        clip: 0,
                    })?;
                    let _ = sink.send(Message::Text(msg)).await;
                    acked = Some((seq, Instant::now()));
                }
                _ = tick.tick() => {
                    let s = since.elapsed().as_secs_f64();
                    let mbit = bytes as f64 * 8.0 / s / 1e6;
                    let delay = percentile(&mut delays, 0.5)
                        .map_or("    - ms".into(), |d| format!("{d:5.0} ms"));
                    println!(
                        "monitor {monitor}: {:5.1} fps  {keys} key  {mbit:6.2} Mbit/s  delay {delay}",
                        frames as f64 / s,
                    );
                    run.add(frames as f64 / s, mbit, keys, &delays);
                    (frames, keys, bytes) = (0, 0, 0);
                    delays.clear();
                    since = Instant::now();
                    if seconds.is_some_and(|n| start.elapsed() >= Duration::from_secs(n)) {
                        run.print();
                        return Ok(());
                    }
                }
            }
        }
        // The server closes a socket that makes no progress. Connect again.
        run.reconnects += 1;
        eprintln!("the socket closed; reconnecting in {RECONNECT:?}");
        tokio::time::sleep(RECONNECT).await;
    }
}

/// The whole run, for the summary line.
#[derive(Default)]
struct Run {
    fps: Vec<f64>,
    mbit: Vec<f64>,
    keys: u32,
    delays: Vec<f64>,
    reconnects: u32,
}

impl Run {
    fn add(&mut self, fps: f64, mbit: f64, keys: u32, delays: &[f64]) {
        self.fps.push(fps);
        self.mbit.push(mbit);
        self.keys += keys;
        self.delays.extend_from_slice(delays);
    }

    fn print(mut self) {
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
        let peak = self.mbit.iter().cloned().fold(0.0, f64::max);
        let median = percentile(&mut self.delays, 0.5);
        let p95 = percentile(&mut self.delays, 0.95);
        let ms = |d: Option<f64>| d.map_or("-".into(), |d| format!("{d:.0}"));
        println!(
            "summary: {:.1} fps  {:.2} Mbit/s mean  {peak:.2} peak  {} key  delay median {} ms  p95 {} ms  ({} echoes)  {} reconnects",
            mean(&self.fps),
            mean(&self.mbit),
            self.keys,
            ms(median),
            ms(p95),
            self.delays.len(),
            self.reconnects,
        );
    }
}

/// The `p` percentile (0..1) of `v`, or `None` when it is empty.
fn percentile(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[((v.len() - 1) as f64 * p).round() as usize])
}
