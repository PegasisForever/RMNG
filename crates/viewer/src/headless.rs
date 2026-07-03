//! Headless mode (CI driver, no display): connect to port 1, decode **each
//! monitor** (`[u32be monitor_id][u32be len][AnnexB]`), report per-monitor fps.
//! `RMNG_DUMP=*.png` writes the first decoded frame as PNG then exits.
//!
//! Port forwarding works here too (no display needed): the server's tag-5
//! `ForwardsMsg` is reconciled into the shared [`crate::forward::ForwardManager`], and
//! status is reported back as port-1 tag-2 frames — the same wiring the GUI uses, so a
//! headless viewer is a full forward endpoint even with no clone selected (no video).

use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use wire::ChromaMode;
use wire::forward::{ForwardStatusMsg, ForwardsMsg};
use wire::viewer::ModeMsg;

use crate::forward::{ForwardManager, StatusReport};

type Counters = Arc<Mutex<HashMap<u32, Arc<AtomicU64>>>>;

/// Server chroma mode from the tag-4 handshake (0 = Yuv420 direct decode, 1 = Yuv444 AVC444
/// `W×2H` stream needing the GL reconstruction). Set before the first AU; read by `make_decoder`.
static CHROMA: AtomicU8 = AtomicU8::new(0);

/// Build a decode pipeline for one monitor; returns its appsrc. The appsink counts frames (and, in
/// dump mode, writes the first one to PNG + exits). In Yuv444 the decoded stream is a stacked
/// `W×2H` NV12 carrying full 4:4:4 — reconstruct it on the GPU via `glupload ! rmngavc444unpack`
/// (the same zero-copy element the GUI uses); `gldownload` is only added to land sysmem for the
/// PNG dump.
fn make_decoder(monitor_id: u32, counter: Arc<AtomicU64>, dump: Option<String>) -> Result<AppSrc> {
    let yuv444 = CHROMA.load(Ordering::Relaxed) == 1;
    let desc = match (yuv444, dump.is_some()) {
        (true, true) => "appsrc name=src is-live=true format=time do-timestamp=true ! \
             h264parse ! vah264dec ! glupload ! rmngavc444unpack ! gldownload ! \
             videoconvert ! pngenc ! appsink name=out emit-signals=true max-buffers=2 sync=false",
        (true, false) => "appsrc name=src is-live=true format=time do-timestamp=true ! \
             h264parse ! vah264dec ! glupload ! rmngavc444unpack ! \
             appsink name=out emit-signals=true max-buffers=4 sync=false",
        (false, true) => "appsrc name=src is-live=true format=time do-timestamp=true ! \
             h264parse ! vah264dec ! videoconvert ! pngenc ! \
             appsink name=out emit-signals=true max-buffers=2 sync=false",
        (false, false) => "appsrc name=src is-live=true format=time do-timestamp=true ! \
             h264parse ! vah264dec ! appsink name=out emit-signals=true max-buffers=4 sync=false",
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

    // Port-forward manager (shared across reconnects). Its status closure frames each
    // update as a port-1 tag-2 `ForwardStatusMsg` over the current write half.
    let writer: crate::Writer = Arc::new(Mutex::new(None));
    let fwd_mgr: Arc<ForwardManager> = {
        let writer = writer.clone();
        let report: StatusReport = Arc::new(move |msg: ForwardStatusMsg| {
            if let Ok(json) = serde_json::to_string(&msg) {
                crate::send_tagged(&writer, 2, json);
            }
        });
        Arc::new(ForwardManager::new(report))
    };

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
                // Write half for viewer→server frames (forward status, tag 2).
                if let Ok(w) = stream.try_clone() {
                    *writer.lock().unwrap() = Some(w);
                }
                tracing::info!("connected; decoding (headless) …");
                let mut tag = [0u8; 1];
                while stream.read_exact(&mut tag).is_ok() {
                    // tags 1 (clipboard) + 2 (cursor) + 3 (layout) + 4 (mode) + 5 (forwards)
                    // are all [u32 len][json]. Tag 4 (chroma) arrives before the first AU;
                    // tag 5 drives the forward listeners; the rest are discarded. (Missing any desyncs.)
                    if matches!(tag[0], 1 | 2 | 3 | 4 | 5) {
                        let mut lb = [0u8; 4];
                        if stream.read_exact(&mut lb).is_err() {
                            break;
                        }
                        let mut body = vec![0u8; u32::from_be_bytes(lb) as usize];
                        if stream.read_exact(&mut body).is_err() {
                            break;
                        }
                        if tag[0] == 4 {
                            if let Ok(m) = serde_json::from_slice::<ModeMsg>(&body) {
                                CHROMA.store(matches!(m.chroma, ChromaMode::Yuv444) as u8, Ordering::Relaxed);
                                tracing::info!("server chroma mode: {:?}", m.chroma);
                            }
                        } else if tag[0] == 5 {
                            // Desired forward set: reconcile local listeners. The data port
                            // lives on the same host as the video port.
                            if let Ok(m) = serde_json::from_slice::<ForwardsMsg>(&body) {
                                let host = addr
                                    .rsplit_once(':')
                                    .map(|(h, _)| h.to_string())
                                    .unwrap_or_else(|| addr.clone());
                                let forward_addr = format!("{host}:{}", m.forward_port);
                                tracing::info!("forwards: {} rule(s) → data port {}", m.rules.len(), m.forward_port);
                                fwd_mgr.reconcile(m.rules, forward_addr);
                            }
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
                *writer.lock().unwrap() = None;
                tracing::info!("disconnected; retrying");
            }
            Err(e) => tracing::warn!("connect {addr} failed: {e}"),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
