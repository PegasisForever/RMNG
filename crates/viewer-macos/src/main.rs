//! `rmng-viewer-macos` — the native macOS viewer: AppKit windows, VideoToolbox decode, Metal
//! render. No GTK, no GStreamer. Feature parity with `crates/viewer` (the GTK viewer, which
//! remains the Linux client and the reference for behaviour).
//!
//!   rmng-viewer-macos                      GUI
//!   rmng-viewer-macos --headless           decode + report per-monitor fps (CI driver)
//!   rmng-viewer-macos --unpack-validate    Metal AVC444 unpack vs the CPU oracle
//!
//! The server address is `~/.config/rmng-viewer/config.json` (shared with the GTK viewer),
//! editable from the title-bar Settings button; `RMNG_VIDEO` only seeds the first run.

mod app;
mod clipboard;
mod cursor;
mod decoder;
mod net;
mod pointer;
mod render;
mod settings;
mod shared;
mod window;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use viewer_core::auto_lock::AutoLock;
use viewer_core::forward::{ForwardManager, StatusReport};
use wire::forward::ForwardStatusMsg;

use shared::{Shared, ViewState, Wake, Writer};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,clip=debug")),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--unpack-validate") {
        let w = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(256);
        let h = args.get(pos + 2).and_then(|s| s.parse().ok()).unwrap_or(144);
        return render::validate_unpack(w, h);
    }
    let headless = args.iter().any(|a| a == "--headless");

    let writer: Writer = Arc::new(Mutex::new(None));
    let forwards = {
        let writer = writer.clone();
        let report: StatusReport = Arc::new(move |msg: ForwardStatusMsg| {
            if let Ok(json) = serde_json::to_string(&msg) {
                shared::send_tagged(&writer, 2, &json);
            }
        });
        Arc::new(ForwardManager::new(report))
    };
    // Headless reads the address from the environment like the GTK headless mode; the GUI reads
    // the persisted config (the Settings dialog edits it live).
    let addr = if headless {
        std::env::var("RMNG_VIDEO").unwrap_or_else(|_| "127.0.0.1:9001".into())
    } else {
        viewer_core::config::load().server_addr
    };
    // Headless fps counters, incremented by the wake hook per decoded frame.
    let counters: Arc<Mutex<HashMap<u32, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    let wake: Box<dyn Fn(Wake) + Send + Sync> = if headless {
        let counters = counters.clone();
        Box::new(move |w| {
            if let Wake::Frame(m) = w {
                *counters.lock().unwrap().entry(m).or_insert(0) += 1;
            }
        })
    } else {
        Box::new(app::wake)
    };
    let shared = Arc::new(Shared {
        writer,
        addr: Arc::new(Mutex::new(addr)),
        chroma: AtomicU8::new(0),
        connected: AtomicBool::new(false),
        view: Mutex::new(ViewState::default()),
        cursors: Mutex::new(HashMap::new()),
        warp: Mutex::new(None),
        auto_lock: Mutex::new(AutoLock::new(Instant::now())),
        clip_inbox: Mutex::new(VecDeque::new()),
        term_out: Mutex::new(VecDeque::new()),
        frames: Mutex::new(HashMap::new()),
        forwards,
        wake,
    });

    {
        let shared = shared.clone();
        std::thread::Builder::new().name("net".into()).spawn(move || net::run(shared))?;
    }
    if headless {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let line: Vec<String> = counters
                .lock()
                .unwrap()
                .iter_mut()
                .map(|(m, c)| {
                    let n = std::mem::take(c);
                    format!("mon{m}={n}")
                })
                .collect();
            if !line.is_empty() {
                tracing::info!("decode fps: {}", line.join(" "));
            }
        }
    }
    app::run(shared)
}
