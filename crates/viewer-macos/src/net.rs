//! The port-1 reader: reconnect loop, tag dispatch, and hardware decode of video AUs.
//!
//! Framing (server → viewer): `[u8 tag]`, then tag 0 = `[u32be monitor][u32be len][AnnexB AU]`;
//! tags 1..=7 = `[u32be len][JSON]` (1 clipboard, 2 cursor, 3 view spec, 4 chroma mode,
//! 5 forwards, 6 reserved, 7 terminal data). A byte-identical port of the GTK viewer's net
//! thread, with the GStreamer appsrc replaced by a VideoToolbox [`Decoder`] per monitor that
//! runs right here on the net thread and lands frames in [`Shared::frames`].

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wire::forward::ForwardsMsg;
use wire::socket::{ClipboardMsg, CursorMeta};
use wire::viewer::{ModeMsg, TermData, ViewSpec};
use wire::ChromaMode;

use crate::decoder::Decoder;
use crate::shared::{Shared, Wake, AGENT_CURSOR_SHOW, WARP_SUPPRESS};

/// Run forever: connect, read frames until the link dies, reconnect after 1 s.
pub fn run(shared: Arc<Shared>) {
    let mut decoders: HashMap<u32, Decoder> = HashMap::new();
    loop {
        let cur = shared.addr.lock().unwrap().clone();
        match TcpStream::connect(&cur) {
            Ok(rd) => {
                rd.set_nodelay(true).ok();
                if let Err(e) = wire::net::set_keepalive(&rd) {
                    tracing::warn!("keepalive setup failed: {e}");
                }
                if let Ok(w) = rd.try_clone() {
                    *shared.writer.lock().unwrap() = Some(w);
                }
                shared.connected.store(true, Ordering::Relaxed);
                (shared.wake)(Wake::View);
                tracing::info!("connected to {cur}");
                serve(&shared, BufReader::new(rd), &mut decoders);
                *shared.writer.lock().unwrap() = None;
                shared.connected.store(false, Ordering::Relaxed);
                (shared.wake)(Wake::View);
                tracing::info!("disconnected; retrying (server force-IDRs on reconnect)");
            }
            Err(e) => tracing::warn!("connect {cur} failed: {e}"),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Read one connection to its end.
fn serve(shared: &Arc<Shared>, mut rd: BufReader<TcpStream>, decoders: &mut HashMap<u32, Decoder>) {
    let mut tag = [0u8; 1];
    while rd.read_exact(&mut tag).is_ok() {
        if matches!(tag[0], 1..=7) {
            let mut lb = [0u8; 4];
            if rd.read_exact(&mut lb).is_err() {
                break;
            }
            let mut body = vec![0u8; u32::from_be_bytes(lb) as usize];
            if rd.read_exact(&mut body).is_err() {
                break;
            }
            handle_json(shared, tag[0], &body);
            continue;
        }
        // Tag 0: a video access unit.
        let mut hdr = [0u8; 8];
        if rd.read_exact(&mut hdr).is_err() {
            break;
        }
        let mid = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
        let len = u32::from_be_bytes(hdr[4..8].try_into().unwrap()) as usize;
        let mut au = vec![0u8; len];
        if rd.read_exact(&mut au).is_err() {
            break;
        }
        let yuv444 = shared.chroma.load(Ordering::Relaxed) == 1;
        // A decoder is bound to one chroma mode; a server that came back in the other mode gets
        // a fresh one (the stream geometry differs: the 4:4:4 stream is double height).
        if decoders.get(&mid).is_some_and(|d| d.yuv444() != yuv444) {
            decoders.remove(&mid);
        }
        let dec = match decoders.get_mut(&mid) {
            Some(d) => d,
            None => {
                let shared2 = shared.clone();
                let sink = Arc::new(move |m: u32, frame| {
                    shared2.frames.lock().unwrap().insert(m, frame);
                    (shared2.wake)(Wake::Frame(m));
                });
                decoders.entry(mid).or_insert_with(|| Decoder::new(mid, yuv444, sink))
            }
        };
        if let Err(e) = dec.decode(&au) {
            tracing::warn!("monitor {mid}: decode error: {e:#}");
        }
    }
}

fn handle_json(shared: &Arc<Shared>, tag: u8, body: &[u8]) {
    match tag {
        4 => {
            if let Ok(m) = serde_json::from_slice::<ModeMsg>(body) {
                let v = matches!(m.chroma, ChromaMode::Yuv444) as u8;
                shared.chroma.store(v, Ordering::Relaxed);
                tracing::info!("server chroma mode: {:?}", m.chroma);
            }
        }
        1 => {
            if let Ok(msg) = serde_json::from_slice::<ClipboardMsg>(body) {
                shared.clip_inbox.lock().unwrap().push_back(msg);
                (shared.wake)(Wake::Data);
            }
        }
        3 => match serde_json::from_slice::<ViewSpec>(body) {
            Ok(spec) => {
                let mut v = shared.view.lock().unwrap();
                if v.spec.as_ref() != Some(&spec) {
                    v.spec = Some(spec);
                    v.epoch = v.epoch.wrapping_add(1);
                    drop(v);
                    (shared.wake)(Wake::View);
                }
            }
            // Not recoverable and not silent: without a spec no window can exist, and the
            // overwhelmingly likely cause is a server running an incompatible build (tag 3 used
            // to be a bare array of monitor placements).
            Err(e) => tracing::error!(
                "tag 3: cannot parse the view spec ({e}) — no window can be built, so no video \
                 will render. The server is probably running an incompatible build; upgrade it. \
                 Payload was: {}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            ),
        },
        5 => {
            if let Ok(m) = serde_json::from_slice::<ForwardsMsg>(body) {
                // The data port lives on the same host as the video port.
                let server = shared.addr.lock().unwrap().clone();
                let host = server.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or(server);
                shared.forwards.reconcile(m.rules, format!("{host}:{}", m.forward_port));
            }
        }
        7 => {
            if let Ok(m) = serde_json::from_slice::<TermData>(body) {
                shared.term_out.lock().unwrap().push_back((m.session, m.data));
                (shared.wake)(Wake::Data);
            }
        }
        2 => {
            if let Ok(c) = serde_json::from_slice::<CursorMeta>(body) {
                cursor_meta(shared, c);
            }
        }
        _ => {}
    }
}

/// Latch a cursor update: visibility for the auto pointer-lock policy, the warp window, and
/// the per-monitor position/shape.
fn cursor_meta(shared: &Arc<Shared>, c: CursorMeta) {
    let now = Instant::now();
    // `hidden` is the daemon's explicit hide marker; an all-zero sprite is the same hide from an
    // older daemon. Position-only updates say nothing about visibility.
    if c.hidden {
        shared.auto_lock.lock().unwrap().on_remote_cursor(true, now);
    } else if let Some(s) = &c.shape {
        let invisible = s.width == 0 || s.height == 0 || s.rgba.iter().all(|&b| b == 0);
        shared.auto_lock.lock().unwrap().on_remote_cursor(invisible, now);
    }
    if c.warp {
        *shared.warp.lock().unwrap() = Some(now + WARP_SUPPRESS);
    }
    let mut map = shared.cursors.lock().unwrap();
    let e = map.entry(c.monitor_id).or_default();
    e.x = c.x;
    e.y = c.y;
    if c.warp {
        e.warp_until = Some(now + AGENT_CURSOR_SHOW);
    }
    if let Some(shape) = c.shape {
        e.version += 1;
        tracing::debug!(
            "cursor meta: mon={} pos=({},{}) warp={} shape {}x{} hot=({},{}) → version {}",
            c.monitor_id, c.x, c.y, c.warp, shape.width, shape.height, shape.hotspot_x, shape.hotspot_y, e.version
        );
        e.shape = Some(shape);
    }
    drop(map);
    (shared.wake)(Wake::Data);
}
