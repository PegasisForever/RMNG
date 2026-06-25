//! Headless mode (CI driver, no display): connect to port 1, decode **each
//! monitor** (`[u32be monitor_id][u32be len][AnnexB]`), report per-monitor fps.
//! `RMNG_DUMP=*.png` writes the first decoded frame as PNG then exits.

use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

type Counters = Arc<Mutex<HashMap<u32, Arc<AtomicU64>>>>;

/// Build a decode pipeline for one monitor; returns its appsrc. The appsink
/// counts frames (and, in dump mode, writes the first one to PNG + exits).
fn make_decoder(monitor_id: u32, counter: Arc<AtomicU64>, dump: Option<String>) -> Result<AppSrc> {
    let desc = if dump.is_some() {
        "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! vah264dec ! videoconvert ! pngenc ! \
         appsink name=out emit-signals=true max-buffers=2 sync=false"
    } else {
        "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! vah264dec ! appsink name=out emit-signals=true max-buffers=4 sync=false"
    };
    let pipeline = gst::parse::launch(desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow!("not a pipeline"))?;
    let appsrc = pipeline.by_name("src").context("appsrc")?.downcast::<AppSrc>().map_err(|_| anyhow!("not appsrc"))?;
    let appsink = pipeline.by_name("out").context("appsink")?.downcast::<AppSink>().map_err(|_| anyhow!("not appsink"))?;
    appsrc.set_caps(Some(
        &gst::Caps::builder("video/x-h264").field("stream-format", "byte-stream").field("alignment", "au").build(),
    ));
    let first = Arc::new(AtomicBool::new(true));
    appsink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                counter.fetch_add(1, Ordering::Relaxed);
                if first.swap(false, Ordering::Relaxed) {
                    if let Some(caps) = sample.caps() {
                        tracing::info!("monitor {monitor_id} first decoded frame: {caps}");
                    }
                    if let Some(path) = &dump {
                        if let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) {
                            let _ = std::fs::write(path, map.as_slice());
                            tracing::info!("wrote {} ({} bytes)", path, map.size());
                        }
                        std::process::exit(0);
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline.set_state(gst::State::Playing)?;
    // Leak the pipeline (kept alive for the process); the appsrc clone drives it.
    std::mem::forget(pipeline);
    Ok(appsrc)
}

pub fn run() -> Result<()> {
    let addr = std::env::var("RMNG_VIDEO").unwrap_or_else(|_| "127.0.0.1:9001".into());
    let dump = std::env::var("RMNG_DUMP").ok();
    let counters: Counters = Arc::new(Mutex::new(HashMap::new()));

    {
        let counters = counters.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let line: Vec<String> = counters
                .lock()
                .unwrap()
                .iter()
                .map(|(m, c)| format!("mon{m}={}", c.swap(0, Ordering::Relaxed)))
                .collect();
            if !line.is_empty() {
                tracing::info!("decode fps: {}", line.join(" "));
            }
        });
    }

    let mut decoders: HashMap<u32, AppSrc> = HashMap::new();
    loop {
        match TcpStream::connect(&addr) {
            Ok(mut stream) => {
                stream.set_nodelay(true).ok();
                tracing::info!("connected; decoding (headless) …");
                let mut tag = [0u8; 1];
                while stream.read_exact(&mut tag).is_ok() {
                    // tags 1 (clipboard) + 2 (cursor) + 3 (layout) are all [u32 len][json] —
                    // discard in headless. (Missing any desyncs the stream.)
                    if tag[0] == 1 || tag[0] == 2 || tag[0] == 3 {
                        let mut lb = [0u8; 4];
                        if stream.read_exact(&mut lb).is_err() {
                            break;
                        }
                        let mut skip = vec![0u8; u32::from_be_bytes(lb) as usize];
                        if stream.read_exact(&mut skip).is_err() {
                            break;
                        }
                        continue;
                    }
                    // video frame [u32 monitor_id][u32 len][AnnexB].
                    let mut hdr = [0u8; 8];
                    if stream.read_exact(&mut hdr).is_err() {
                        break;
                    }
                    let monitor_id = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
                    let len = u32::from_be_bytes(hdr[4..8].try_into().unwrap()) as usize;
                    let mut au = vec![0u8; len];
                    if stream.read_exact(&mut au).is_err() {
                        break;
                    }
                    let appsrc = match decoders.get(&monitor_id) {
                        Some(a) => a,
                        None => {
                            let counter = Arc::new(AtomicU64::new(0));
                            counters.lock().unwrap().insert(monitor_id, counter.clone());
                            match make_decoder(monitor_id, counter, dump.clone()) {
                                Ok(a) => decoders.entry(monitor_id).or_insert(a),
                                Err(e) => {
                                    tracing::error!("decoder for monitor {monitor_id}: {e}");
                                    continue;
                                }
                            }
                        }
                    };
                    if appsrc.push_buffer(gst::Buffer::from_mut_slice(au)).is_err() {
                        break;
                    }
                }
                tracing::info!("disconnected; retrying");
            }
            Err(e) => tracing::warn!("connect {addr} failed: {e}"),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
