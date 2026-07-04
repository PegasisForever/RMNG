//! Port 1 — the media plane (the real version of the `media-server` test bin).
//! Also backs the **desktop MCP tools** (port 3/4): it keeps each clone's daemon
//! connection (input relay) + latest dmabuf frame (screenshots) in a shared
//! [`MediaHandle`].
//!
//! - Clone socket (bind-mounted): daemons connect + `Hello{clone_id}`, then stream
//!   per-monitor dmabuf frames; frames feed the encoders only while their clone is
//!   `selected`. Each frame is acked (1-deep flow control).
//! - Port 1 (TCP): the viewer gets the selected clone's H.264, **one encoder per
//!   monitor**, framed `[u32be monitor_id][u32be len][AnnexB]`. Viewer input
//!   (`[u32be len][JSON InputMsg]`) is relayed to that clone.
//! - Selection switch / viewer connect → force a fresh IDR on every encoder.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;

use tokio::sync::broadcast::error::RecvError;

use media::{Conn, Encoder, Listener};
use wire::ChromaMode;
use wire::socket::{
    Ack, ClipboardData, ClipboardMsg, ClipboardOffer, ClipboardRequest, CursorMeta, DaemonMsg,
    InputMsg, MonitorPlacement, ServerMsg,
};
use wire::viewer::ModeMsg;

use crate::app::App;

/// One connected viewer: its id + the sender into its per-viewer writer thread.
/// The writer thread owns the socket; producers only ever `try_send` here, so a slow
/// viewer's socket never blocks the encode or metadata threads.
struct ViewerConn {
    id: u64,
    tx: SyncSender<Arc<[u8]>>,
}

/// The live set of viewers, keyed by id. Held under a short-lived lock only to
/// snapshot/insert/remove; all sends are non-blocking `try_send`.
type Viewers = Arc<Mutex<HashMap<u64, ViewerConn>>>;

/// Bounded per-viewer queue depth. `try_send` overflow disconnects the viewer (it
/// reconnects + re-primes). Bounds worst-case queued memory at CAP × max-AU per slow
/// viewer; tune here if joins on very bursty desktops need more slack.
const VIEWER_CHAN_CAP: usize = 128;

/// Monotonic per-process viewer id.
fn next_viewer_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Fan one pre-framed message out to every viewer. A viewer whose channel is full
/// (too slow) or disconnected (writer already gone) is removed from the registry;
/// dropping its stored `tx` lets its writer thread's channel disconnect, which tears
/// the viewer down. Never blocks: `try_send` is non-blocking.
fn broadcast_bytes(viewers: &Viewers, buf: &Arc<[u8]>) {
    let mut guard = viewers.lock().unwrap();
    let mut dead: Vec<u64> = Vec::new();
    for v in guard.values() {
        if v.tx.try_send(buf.clone()).is_err() {
            dead.push(v.id);
        }
    }
    for id in dead {
        // NB: if this viewer's writer thread is currently blocked in `write_all` on a
        // full socket send buffer (not a full channel — that's what got it removed
        // here), dropping `tx` doesn't wake it. Its writer + reader threads and the
        // forward ref-count are only reclaimed once the socket itself errors, which
        // the keepalive backstop guarantees within ~20 s.
        guard.remove(&id);
    }
}

/// Send one pre-framed message to a single viewer; remove it on failure.
fn send_bytes_to(viewers: &Viewers, id: u64, buf: Arc<[u8]>) {
    let mut guard = viewers.lock().unwrap();
    if let Some(v) = guard.get(&id) {
        if v.tx.try_send(buf).is_err() {
            guard.remove(&id);
        }
    }
}

/// Idempotently remove a viewer (dropping its `tx`).
fn remove_viewer(viewers: &Viewers, id: u64) {
    viewers.lock().unwrap().remove(&id);
}

/// Central clipboard broker state (rich + lazy). One current `offer` (the selection
/// owner) + the in-flight `pending` requests so `Data` replies route back to whoever
/// asked. Source/requester ids are clone ids or a per-viewer [`viewer_src`] id.
#[derive(Default)]
struct ClipState {
    offer: Option<(ClipboardOffer, String)>, // (offer, owner source)
    pending: HashMap<(u64, String), Vec<String>>, // (serial, mime) → requesters
}

struct LatestFrame {
    monitor_id: u32,
    fd: OwnedFd,
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
}

/// Per-viewer clipboard source id. Distinct per connection so the lazy broker can
/// route a `Request` to the exact viewer that owns the current offer and a `Data`
/// reply back to the exact requester.
fn viewer_src(id: u64) -> String {
    format!("\0viewer:{id}")
}

/// Parse a viewer source id back to its numeric id; `None` for clone ids.
fn viewer_src_id(s: &str) -> Option<u64> {
    s.strip_prefix("\0viewer:").and_then(|n| n.parse().ok())
}

/// A source (a leaving viewer, or a departed clone) is gone: if it owns the current
/// offer, drop the offer; remove it from every pending requester list, discarding
/// lists that become empty.
fn clip_forget_source(clip: &Mutex<ClipState>, src: &str) {
    let mut clip = clip.lock().unwrap();
    if clip.offer.as_ref().is_some_and(|(_, owner)| owner == src) {
        clip.offer = None;
    }
    clip.pending.retain(|_, requesters| {
        requesters.retain(|r| r != src);
        !requesters.is_empty()
    });
}

#[derive(Default)]
pub struct MediaHandle {
    conns: Mutex<HashMap<String, Arc<Conn>>>,
    /// clone id → (monitor_id → its latest dmabuf frame). Per-monitor so every monitor
    /// of a multi-monitor clone can be primed on viewer connect (not just the last one).
    latest: Mutex<HashMap<String, HashMap<u32, LatestFrame>>>,
    /// Rich/lazy clipboard broker state.
    clip: Mutex<ClipState>,
    /// Most-recent cursor *with a shape* per clone — replayed to a viewer that
    /// connects mid-session (shapes are otherwise only sent on change).
    cursor: Mutex<HashMap<String, CursorMeta>>,
    /// Each clone's reported monitor layout — replayed to a connecting viewer so it
    /// can route cross-window drags against the real positions.
    layout: Mutex<HashMap<String, Vec<MonitorPlacement>>>,
}

fn dup_owned(fd: &OwnedFd) -> Option<OwnedFd> {
    let raw = nix::unistd::dup(fd.as_raw_fd()).ok()?;
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}

impl MediaHandle {
    pub fn send_input(&self, clone: &str, input: InputMsg) -> Result<(), String> {
        let conn = self.conns.lock().unwrap().get(clone).cloned();
        match conn {
            Some(c) => c.send(&ServerMsg::Input(input)).map_err(|e| e.to_string()),
            None => Err(format!("clone '{clone}' not connected")),
        }
    }
    // NB: on-demand screenshots moved into the clone-daemon's own MCP (the fleet MCP
    // proxies to it); the control-server no longer encodes screenshots itself.

    /// True when a clone-daemon session for `id` is live — i.e. its daemon has sent a
    /// `Hello{clone_id}` (keyed by clone_id == the clone's hostname == its host id) and
    /// the connection hasn't dropped. This is the readiness signal `provision.rs`'s
    /// clone-container wait-ready poll watches: a clone is "up + registered" once its
    /// daemon appears here.
    pub fn is_connected(&self, id: &str) -> bool {
        self.conns.lock().unwrap().contains_key(id)
    }
}

/// monitor_id → its H.264 encoder (lazily created for the selected clone).
/// monitor_id → (encoder, width, height). The size is tracked so a resolution change
/// (a clone reprovisioned/restarted with different `RMNG_MONITORS`) rebuilds the
/// encoder pipeline fresh — mid-stream VA-encoder resolution renegotiation is unreliable.
type Encoders = Arc<Mutex<HashMap<u32, (Arc<Encoder>, u32, u32)>>>;

pub fn spawn(app: App) {
    // `spawn` is called from the async `main`, so a tokio runtime exists here; capture its
    // handle so the std-thread media plane can `block_on` async control-plane calls
    // (`App::dial_host`) from the forward-data serve threads.
    let rt_handle = tokio::runtime::Handle::current();
    if let Err(e) = media::init() {
        tracing::error!("media init failed; port 1 disabled: {e}");
        return;
    }
    let cfg = app.config();
    let video_port = cfg.listen.video;
    let forward_port = cfg.listen.forward;
    let sock_path = cfg.clone_socket.clone();
    // Chroma mode is global + fixed at launch (restart-required); snapshot it once for
    // the accept loop so every viewer connect uses the same value the encoders were
    // built with, rather than re-reading it live per connect.
    let chroma = cfg.chroma;
    let handle = app.media.clone();
    let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
    let encoders: Encoders = Arc::new(Mutex::new(HashMap::new()));
    let last_sel: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Port 1 TCP: viewers + their input. Each viewer gets a bounded channel + a writer
    // thread; producers only `try_send`, so a slow viewer never blocks the encoders.
    {
        let (viewers, handle, app, encoders) =
            (viewers.clone(), handle.clone(), app.clone(), encoders.clone());
        std::thread::spawn(move || match TcpListener::bind(("0.0.0.0", video_port)) {
            Ok(l) => {
                tracing::info!("port 1 (video) listening on 0.0.0.0:{video_port}");
                for stream in l.incoming().flatten() {
                    let _ = stream.set_nodelay(true);
                    // Keepalive so a *silently* dead viewer (Wi-Fi/route change, suspend,
                    // a killed client) is torn down in ~20 s. Without it the per-viewer
                    // `read_viewer_input` thread below blocks on its read forever, leaking
                    // one thread per dropped viewer; the keepalive surfaces the death as a
                    // read error so the thread exits.
                    if let Err(e) = wire::net::set_keepalive(&stream) {
                        tracing::warn!("viewer keepalive setup failed: {e}");
                    }
                    tracing::info!("viewer connected: {:?}", stream.peer_addr());
                    let read_half = match stream.try_clone() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("viewer clone failed: {e}");
                            continue;
                        }
                    };
                    let id = next_viewer_id();
                    app.forwards.viewer_joined();

                    // Writer thread: drain the channel to the socket; shut down on any
                    // exit (write error OR channel disconnect) to wake the reader.
                    let (tx, rx) = std::sync::mpsc::sync_channel::<Arc<[u8]>>(VIEWER_CHAN_CAP);
                    let mut write_half = stream;
                    std::thread::spawn(move || {
                        while let Ok(buf) = rx.recv() {
                            if write_half.write_all(&buf).is_err() {
                                break;
                            }
                        }
                        let _ = write_half.shutdown(std::net::Shutdown::Both);
                    });

                    let state = app.store.get();
                    let selected = app.store.selected();
                    // Queue mode + metadata straight into this viewer's channel (mode is first).
                    prime_viewer_metadata(&handle, &tx, selected.clone(), chroma, &state, forward_port);
                    // Register BEFORE the video re-encode so the keyframe reliably fans to this viewer
                    // (independent of encode latency); mode already leads its channel.
                    viewers.lock().unwrap().insert(id, ViewerConn { id, tx });
                    prime_viewer_video(&handle, &encoders, &viewers, selected, chroma);

                    // Reader thread owns teardown.
                    let (handle, app, viewers) = (handle.clone(), app.clone(), viewers.clone());
                    std::thread::spawn(move || read_viewer_input(read_half, handle, app, viewers, id));
                }
            }
            Err(e) => tracing::error!("port 1 bind {video_port} failed: {e}"),
        });
    }

    // Port `forward` (data plane): one TCP connection per forwarded local socket.
    {
        let (app, rt_handle) = (app.clone(), rt_handle.clone());
        std::thread::spawn(move || match TcpListener::bind(("0.0.0.0", forward_port)) {
            Ok(l) => {
                tracing::info!("forward data plane listening on 0.0.0.0:{forward_port}");
                for stream in l.incoming().flatten() {
                    let _ = stream.set_nodelay(true);
                    let (app, rt_handle) = (app.clone(), rt_handle.clone());
                    std::thread::spawn(move || serve_forward(stream, app, rt_handle));
                }
            }
            Err(e) => tracing::error!("forward bind {forward_port} failed: {e}"),
        });
    }

    // Repaint the viewer the instant the *selected* clone changes, without waiting for the
    // next incoming frame. `/api/activate` only mutates the store; serve_clone's frame loop
    // otherwise notices the switch lazily — on the next frame from *any* clone — which on a
    // static, damage-driven desktop can be seconds away (the reported "few seconds before the
    // new host appears"). Watch the store's change bus and, the moment `selected` flips, force
    // a fresh IDR on every encoder + re-prime from the newly-selected clone's last frame /
    // cursor / layout. Shares `last_sel` with the frame loop (same mutex, same lock order
    // last_sel → encoders/viewer), so the two paths serialize and never double-prime.
    {
        let (app, encoders, viewers, last_sel, handle) =
            (app.clone(), encoders.clone(), viewers.clone(), last_sel.clone(), handle.clone());
        std::thread::spawn(move || {
            let (_seed, mut rx) = app.store.subscribe();
            loop {
                match rx.blocking_recv() {
                    // Any state mutation (or a lag) → re-check the selection; act only on a change.
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        // A config change (or any mutation) may have altered forwards —
                        // re-push the full set; each viewer reconciles idempotently.
                        let state = app.store.get();
                        if !viewers.lock().unwrap().is_empty() {
                            broadcast_forwards(&viewers, &state, forward_port);
                        }
                        let sel = app.store.selected();
                        let mut ls = last_sel.lock().unwrap();
                        if *ls == sel {
                            continue;
                        }
                        *ls = sel.clone();
                        // No viewers attached → nothing to repaint; the connect path primes on connect.
                        if viewers.lock().unwrap().is_empty() {
                            continue;
                        }
                        reprime_all(&handle, &encoders, &viewers, sel, chroma);
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    if let Some(dir) = std::path::Path::new(&sock_path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = match Listener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("clone socket bind {sock_path} failed: {e}");
            return;
        }
    };
    tracing::info!("clone media socket listening on {sock_path}");
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok(conn) => {
                    let (handle, app, encoders, viewers, last_sel) =
                        (handle.clone(), app.clone(), encoders.clone(), viewers.clone(), last_sel.clone());
                    std::thread::spawn(move || serve_clone(conn, handle, app, encoders, viewers, last_sel));
                }
                Err(e) => {
                    tracing::error!("clone accept failed: {e}");
                    break;
                }
            }
        }
    });
}

/// Port-1 frame types (1-byte tag prefix). 0 = video, 1 = clipboard, 2 = cursor,
/// 3 = layout, 4 = mode (chroma handshake, sent once at connect before any video).
const T_VIDEO: u8 = 0;
const T_CLIPBOARD: u8 = 1;
const T_CURSOR: u8 = 2;
const T_LAYOUT: u8 = 3;
const T_MODE: u8 = 4;
/// Server→viewer tag 5: the desired forward set (`[5][u32be len][JSON ForwardsMsg]`).
const T_FORWARDS: u8 = 5;
/// Viewer→server tag 2: a forward rule's status changed (`[2][u32be len][JSON ForwardStatusMsg]`).
const T_FORWARD_STATUS: u8 = 2;

/// Build the viewer's desired forward set: the union of every host's *enabled* rules,
/// each tagged with its host id, plus the data port.
fn build_forwards_msg(state: &wire::ControlState, forward_port: u16) -> wire::forward::ForwardsMsg {
    let rules = state
        .hosts
        .iter()
        .flat_map(|h| {
            h.forwards.iter().filter(|f| f.enabled).map(move |f| wire::forward::ForwardRule {
                host_id: h.id.clone(),
                id: f.id.clone(),
                remote_port: f.remote_port,
                local_port: f.local_port,
            })
        })
        .collect();
    wire::forward::ForwardsMsg { forward_port, rules }
}

/// Broadcast the current desired forward set to every viewer (port-1 tag 5).
fn broadcast_forwards(viewers: &Viewers, state: &wire::ControlState, forward_port: u16) {
    broadcast_json(viewers, T_FORWARDS, &build_forwards_msg(state, forward_port));
}

/// Pre-frame one H.264 AU: `[0u8][u32be monitor_id][u32be len][AnnexB]`. Built once
/// and cloned (refcount) into each viewer's channel.
fn frame_video(monitor_id: u32, au: &[u8]) -> Arc<[u8]> {
    let mut framed = Vec::with_capacity(9 + au.len());
    framed.push(T_VIDEO);
    framed.extend_from_slice(&monitor_id.to_be_bytes());
    framed.extend_from_slice(&(au.len() as u32).to_be_bytes());
    framed.extend_from_slice(au);
    Arc::from(framed)
}

/// Pre-frame a tagged JSON message: `[tag][u32be len][json]`. `None` if serialization
/// fails (never for our types).
fn frame_json<T: serde::Serialize>(tag: u8, msg: &T) -> Option<Arc<[u8]>> {
    let json = serde_json::to_vec(msg).ok()?;
    let mut framed = Vec::with_capacity(5 + json.len());
    framed.push(tag);
    framed.extend_from_slice(&(json.len() as u32).to_be_bytes());
    framed.extend_from_slice(&json);
    Some(Arc::from(framed))
}

/// Fan one AU out to every viewer.
fn broadcast_video(viewers: &Viewers, monitor_id: u32, au: &[u8]) {
    broadcast_bytes(viewers, &frame_video(monitor_id, au));
}

/// Fan one tagged JSON message out to every viewer.
fn broadcast_json<T: serde::Serialize>(viewers: &Viewers, tag: u8, msg: &T) {
    if let Some(buf) = frame_json(tag, msg) {
        broadcast_bytes(viewers, &buf);
    }
}

/// Send a clipboard message to one endpoint (a clone by id, or a viewer by src id).
fn send_clip_to(handle: &MediaHandle, viewers: &Viewers, dest: &str, msg: ClipboardMsg) {
    if let Some(id) = viewer_src_id(dest) {
        if let Some(buf) = frame_json(T_CLIPBOARD, &msg) {
            send_bytes_to(viewers, id, buf);
        }
        return;
    }
    let conn = handle.conns.lock().unwrap().get(dest).cloned();
    if let Some(c) = conn {
        let server_msg = match msg {
            ClipboardMsg::Offer(o) => ServerMsg::ClipboardOffer(o),
            ClipboardMsg::Request(r) => ServerMsg::ClipboardRequest(r),
            ClipboardMsg::Data(d) => ServerMsg::ClipboardData(d),
        };
        let _ = c.send(&server_msg);
    }
}

/// Every clipboard destination except `source`: all *other* clones + all *other*
/// viewers (remote↔local + remote↔remote + viewer↔viewer).
fn clip_dests(handle: &MediaHandle, viewers: &Viewers, source: &str) -> Vec<String> {
    let mut dests: Vec<String> =
        handle.conns.lock().unwrap().keys().cloned().filter(|id| id != source).collect();
    for id in viewers.lock().unwrap().keys() {
        let src = viewer_src(*id);
        if src != source {
            dests.push(src);
        }
    }
    dests
}

/// Broker: a new selection is offered by `source`. Becomes the current clipboard;
/// advertise it to every other clone + every other viewer.
fn broker_offer(handle: &MediaHandle, viewers: &Viewers, offer: ClipboardOffer, source: &str) {
    tracing::debug!(target: "clip", "offer serial={} from {source:?}: {:?}", offer.serial, offer.mime_types);
    handle.clip.lock().unwrap().offer = Some((offer.clone(), source.to_string()));
    let dests = clip_dests(handle, viewers, source);
    for d in dests {
        send_clip_to(handle, viewers, &d, ClipboardMsg::Offer(offer.clone()));
    }
}

/// Broker: `requester` wants the current owner's bytes for a MIME — record it and
/// forward the request to the owner (lazy fetch).
fn broker_request(handle: &MediaHandle, viewers: &Viewers, req: ClipboardRequest, requester: &str) {
    let owner = {
        let mut clip = handle.clip.lock().unwrap();
        let Some((_, owner)) = clip.offer.clone() else {
            tracing::debug!(target: "clip", "request serial={} mime={} from {requester:?} dropped: no offer", req.serial, req.mime_type);
            return;
        };
        if owner == requester {
            return; // don't ask the owner to fetch from itself
        }
        clip.pending.entry((req.serial, req.mime_type.clone())).or_default().push(requester.to_string());
        owner
    };
    tracing::debug!(target: "clip", "request serial={} mime={} from {requester:?} -> owner {owner:?}", req.serial, req.mime_type);
    send_clip_to(handle, viewers, &owner, ClipboardMsg::Request(req));
}

/// Broker: the owner returned bytes — deliver to everyone who requested them.
fn broker_data(handle: &MediaHandle, viewers: &Viewers, data: ClipboardData) {
    let requesters = handle
        .clip
        .lock()
        .unwrap()
        .pending
        .remove(&(data.serial, data.mime_type.clone()))
        .unwrap_or_default();
    tracing::debug!(target: "clip",
        "data serial={} mime={} ({} bytes) -> {requesters:?}",
        data.serial, data.mime_type, data.bytes.len()
    );
    for r in requesters {
        send_clip_to(handle, viewers, &r, ClipboardMsg::Data(data.clone()));
    }
}

fn force_idr_all(encoders: &Encoders) {
    for (e, _, _) in encoders.lock().unwrap().values() {
        e.force_idr();
    }
}

/// Get-or-create the encoder for a monitor at `w`×`h`; its AUs are framed with
/// `monitor_id` and broadcast to every viewer. If an encoder exists at a different
/// resolution it is rebuilt fresh.
fn encoder_for(
    encoders: &Encoders,
    viewers: &Viewers,
    monitor_id: u32,
    w: u32,
    h: u32,
    chroma: ChromaMode,
) -> Option<Arc<Encoder>> {
    let mut map = encoders.lock().unwrap();
    if let Some((e, ew, eh)) = map.get(&monitor_id) {
        if *ew == w && *eh == h {
            return Some(e.clone());
        }
        tracing::info!("monitor {monitor_id} resolution {ew}x{eh} → {w}x{h}; rebuilding encoder");
    }
    let viewers = viewers.clone();
    match Encoder::new(chroma, move |au, _idr| broadcast_video(&viewers, monitor_id, &au)) {
        Ok(e) => {
            let e = Arc::new(e);
            map.insert(monitor_id, (e.clone(), w, h));
            Some(e)
        }
        Err(err) => {
            tracing::error!("encoder for monitor {monitor_id} init failed: {err}");
            None
        }
    }
}

/// Prime a freshly-connected viewer's metadata: push its mode + the selected clone's
/// last-known layout/cursor/clipboard offer + the forward set straight into its own
/// channel (mode FIRST so the viewer picks its decode path before any AU). Touches
/// only this viewer's `tx` — no encoder, no registry, so it's safe to call before the
/// viewer is inserted into `viewers`.
fn prime_viewer_metadata(
    handle: &MediaHandle,
    tx: &SyncSender<Arc<[u8]>>,
    selected: Option<String>,
    chroma: ChromaMode,
    state: &wire::ControlState,
    forward_port: u16,
) {
    // Mode FIRST so the viewer picks its decode path before any AU.
    if let Some(b) = frame_json(T_MODE, &ModeMsg { chroma }) {
        let _ = tx.try_send(b);
    }
    // Forward set so it opens its listeners.
    if let Some(b) = frame_json(T_FORWARDS, &build_forwards_msg(state, forward_port)) {
        let _ = tx.try_send(b);
    }
    let Some(sel) = selected else { return };
    // Layout + last cursor shape + current clipboard offer, targeted to this viewer.
    if let Some(l) = handle.layout.lock().unwrap().get(&sel).cloned() {
        if let Some(b) = frame_json(T_LAYOUT, &l) {
            let _ = tx.try_send(b);
        }
    }
    if let Some(c) = handle.cursor.lock().unwrap().get(&sel).cloned() {
        if let Some(b) = frame_json(T_CURSOR, &c) {
            let _ = tx.try_send(b);
        }
    }
    if let Some((offer, _)) = handle.clip.lock().unwrap().offer.clone() {
        if let Some(b) = frame_json(T_CLIPBOARD, &ClipboardMsg::Offer(offer)) {
            let _ = tx.try_send(b);
        }
    }
}

/// Prime a freshly-connected viewer's video: force a shared keyframe and re-encode
/// the selected clone's last frame per monitor so it paints at once even on a static,
/// damage-driven desktop. The re-encode BROADCASTS via the shared encoder (existing
/// viewers get one redundant keyframe per join — negligible), so the caller must
/// register the new viewer in `viewers` before calling this — otherwise the new
/// viewer's own first keyframe would depend on beating the encode (ms) against the
/// registry insert (µs), a latent race. (`force_idr` only forces the *next* pushed
/// frame to be a keyframe; it does not retroactively mark frames already encoded.)
fn prime_viewer_video(
    handle: &MediaHandle,
    encoders: &Encoders,
    viewers: &Viewers,
    selected: Option<String>,
    chroma: ChromaMode,
) {
    let Some(sel) = selected else { return };
    // Video: re-encode each monitor's latest frame (broadcasts via the shared encoder).
    let frames: Vec<(u32, OwnedFd, u32, u64, u32, u32)> = {
        let latest = handle.latest.lock().unwrap();
        latest
            .get(&sel)
            .map(|m| {
                m.values()
                    .filter_map(|f| {
                        dup_owned(&f.fd).map(|fd| (f.monitor_id, fd, f.fourcc, f.modifier, f.width, f.height))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for (mid, fd, fourcc, modifier, w, h) in frames {
        if let Some(enc) = encoder_for(encoders, viewers, mid, w, h, chroma) {
            enc.force_idr();
            if let Err(e) = enc.push(fd, fourcc, modifier, w, h) {
                tracing::warn!("prime re-encode failed: {e}");
            }
        }
    }
}

/// Re-prime ALL viewers after a selection change: force fresh keyframes + rebroadcast
/// the newly-selected clone's last frame / cursor / layout to everyone.
fn reprime_all(
    handle: &MediaHandle,
    encoders: &Encoders,
    viewers: &Viewers,
    selected: Option<String>,
    chroma: ChromaMode,
) {
    force_idr_all(encoders);
    let Some(sel) = selected else { return };
    if let Some(l) = handle.layout.lock().unwrap().get(&sel).cloned() {
        broadcast_json(viewers, T_LAYOUT, &l);
    }
    if let Some(c) = handle.cursor.lock().unwrap().get(&sel).cloned() {
        broadcast_json(viewers, T_CURSOR, &c);
    }
    let frames: Vec<(u32, OwnedFd, u32, u64, u32, u32)> = {
        let latest = handle.latest.lock().unwrap();
        latest
            .get(&sel)
            .map(|m| {
                m.values()
                    .filter_map(|f| {
                        dup_owned(&f.fd).map(|fd| (f.monitor_id, fd, f.fourcc, f.modifier, f.width, f.height))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for (mid, fd, fourcc, modifier, w, h) in frames {
        if let Some(enc) = encoder_for(encoders, viewers, mid, w, h, chroma) {
            enc.push(fd, fourcc, modifier, w, h).ok();
        }
    }
}

fn serve_clone(
    conn: Conn,
    handle: Arc<MediaHandle>,
    app: App,
    encoders: Encoders,
    viewers: Viewers,
    last_sel: Arc<Mutex<Option<String>>>,
) {
    let conn = Arc::new(conn);
    let mut clone_id: Option<String> = None;
    // Chroma mode is global + fixed at launch; snapshot it once for this clone session.
    let chroma = app.config().chroma;
    loop {
        match conn.recv() {
            Ok((DaemonMsg::Hello(h), _)) => {
                tracing::info!("clone-daemon '{}' connected", h.clone_id);
                handle.conns.lock().unwrap().insert(h.clone_id.clone(), conn.clone());
                clone_id = Some(h.clone_id);
            }
            Ok((DaemonMsg::Frame(f), fds)) => {
                let Some(id) = clone_id.clone() else { continue };
                if let Some(fd) = fds.into_iter().next() {
                    if let Some(dup) = dup_owned(&fd) {
                        handle.latest.lock().unwrap().entry(id.clone()).or_default().insert(
                            f.monitor_id,
                            LatestFrame { monitor_id: f.monitor_id, fd: dup, fourcc: f.fourcc, modifier: f.modifier, width: f.width, height: f.height },
                        );
                    }
                    let sel = app.store.selected();
                    {
                        let mut ls = last_sel.lock().unwrap();
                        if *ls != sel {
                            *ls = sel.clone();
                            // Repaint every viewer from the newly-selected clone's last frame + cursor.
                            reprime_all(&handle, &encoders, &viewers, sel.clone(), chroma);
                        }
                    }
                    if sel.as_deref() == Some(id.as_str()) {
                        if let Some(enc) = encoder_for(&encoders, &viewers, f.monitor_id, f.width, f.height, chroma) {
                            if let Err(e) = enc.push(fd, f.fourcc, f.modifier, f.width, f.height) {
                                tracing::warn!("encode push failed: {e}");
                            }
                        }
                    }
                    let _ = conn.send(&ServerMsg::Ack(Ack { monitor_id: f.monitor_id, seq: f.seq }));
                }
            }
            Ok((DaemonMsg::ClipboardOffer(o), _)) => {
                if let Some(id) = clone_id.clone() {
                    broker_offer(&handle, &viewers, o, &id);
                }
            }
            Ok((DaemonMsg::ClipboardRequest(r), _)) => {
                if let Some(id) = clone_id.clone() {
                    broker_request(&handle, &viewers, r, &id);
                }
            }
            Ok((DaemonMsg::ClipboardData(d), _)) => {
                broker_data(&handle, &viewers, d);
            }
            Ok((DaemonMsg::Cursor(c), _)) => {
                if let Some(id) = clone_id.clone() {
                    // Remember the last cursor *with a shape* to replay on viewer connect.
                    if c.shape.is_some() {
                        handle.cursor.lock().unwrap().insert(id.clone(), c.clone());
                    }
                    // Only the selected clone's cursor reaches the viewers.
                    if app.store.selected().as_deref() == Some(id.as_str()) {
                        broadcast_json(&viewers, T_CURSOR, &c);
                    }
                }
            }
            Ok((DaemonMsg::Layout { monitors: l }, _)) => {
                if let Some(id) = clone_id.clone() {
                    handle.layout.lock().unwrap().insert(id.clone(), l.clone());
                    if app.store.selected().as_deref() == Some(id.as_str()) {
                        broadcast_json(&viewers, T_LAYOUT, &l);
                    }
                }
            }
            Err(e) => {
                if let Some(id) = &clone_id {
                    teardown_if_current(&handle.conns, &handle.latest, id, &conn);
                    clip_forget_source(&handle.clip, id);
                    tracing::info!("clone-daemon '{id}' disconnected: {e}");
                }
                break;
            }
        }
    }
}

/// Disconnect teardown for one clone session: remove `id` from `conns`/`latest` ONLY if
/// the `conns` entry is still `this` exact connection (`Arc::ptr_eq`). A hot-swap bounces
/// the daemon unit, so the replacement daemon can reconnect + `Hello` (re-inserting under
/// the same id) before the old session's blocking `recv()` finally errors into this path
/// — the identity check keeps that late teardown from clobbering the new session.
///
/// `latest` is removed while the `conns` lock is still held: a new session can only prime
/// `latest[id]` after its `Hello` inserted into `conns` (which needs this lock), so no
/// fresh frame can appear between the identity check and either removal. No other site
/// nests these two locks (verified: [`prime_viewer_video`] holds `latest` in a scoped
/// block that never touches `conns`), so the conns→latest order cannot invert.
fn teardown_if_current(
    conns: &Mutex<HashMap<String, Arc<Conn>>>,
    latest: &Mutex<HashMap<String, HashMap<u32, LatestFrame>>>,
    id: &str,
    this: &Arc<Conn>,
) {
    let mut conns = conns.lock().unwrap();
    if conns.get(id).is_some_and(|c| Arc::ptr_eq(c, this)) {
        conns.remove(id);
        latest.lock().unwrap().remove(id);
    }
}

/// Viewer → server: `[u8 type][u32be len][JSON]`. type 0 = InputMsg (to the selected
/// clone), type 1 = ClipboardData (broker fans it out), type 2 = ForwardStatusMsg.
/// This thread is the SOLE owner of the viewer's teardown.
fn read_viewer_input(mut sock: TcpStream, handle: Arc<MediaHandle>, app: App, viewers: Viewers, id: u64) {
    let src = viewer_src(id);
    let mut tag = [0u8; 1];
    let mut hdr = [0u8; 4];
    while sock.read_exact(&mut tag).is_ok() {
        if sock.read_exact(&mut hdr).is_err() {
            break;
        }
        let len = u32::from_be_bytes(hdr) as usize;
        if len > 1 << 20 {
            break;
        }
        let mut body = vec![0u8; len];
        if sock.read_exact(&mut body).is_err() {
            break;
        }
        match tag[0] {
            T_VIDEO => {
                let Ok(input) = serde_json::from_slice::<InputMsg>(&body) else { continue };
                let Some(clone) = app.store.selected() else { continue };
                let _ = handle.send_input(&clone, input);
            }
            T_CLIPBOARD => {
                if let Ok(msg) = serde_json::from_slice::<ClipboardMsg>(&body) {
                    match msg {
                        ClipboardMsg::Offer(o) => broker_offer(&handle, &viewers, o, &src),
                        ClipboardMsg::Request(r) => broker_request(&handle, &viewers, r, &src),
                        ClipboardMsg::Data(d) => broker_data(&handle, &viewers, d),
                    }
                }
            }
            T_FORWARD_STATUS => {
                if let Ok(msg) = serde_json::from_slice::<wire::forward::ForwardStatusMsg>(&body) {
                    app.forwards.report(msg);
                }
            }
            _ => break,
        }
    }
    // Sole teardown: drop from the registry (stops fan-out, disconnects the writer),
    // forget this viewer's clipboard state, and release its forward ref-count.
    remove_viewer(&viewers, id);
    clip_forget_source(&handle.clip, &src);
    app.forwards.viewer_left();
}

/// Read the data-plane header: `[u32be len][JSON ForwardHeader]` (len capped at 64 KiB).
fn read_forward_header(stream: &mut TcpStream) -> std::io::Result<wire::forward::ForwardHeader> {
    let mut lb = [0u8; 4];
    stream.read_exact(&mut lb)?;
    let len = u32::from_be_bytes(lb) as usize;
    if len > 64 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "forward header too large"));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Bidirectionally pipe two TCP streams until both close. Each direction runs in its own
/// thread and half-closes its peer's write side on EOF so the reverse direction drains.
fn splice_forward(a: TcpStream, b: TcpStream) {
    let (mut a_rd, mut b_wr) = match (a.try_clone(), b.try_clone()) {
        (Ok(x), Ok(y)) => (x, y),
        _ => return,
    };
    let (mut b_rd, mut a_wr) = (b, a);
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_rd, &mut b_wr);
        let _ = b_wr.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b_rd, &mut a_wr);
    let _ = a_wr.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
}

/// One forwarded connection: read the header, resolve + dial the clone port, reply a
/// status byte, then splice. `0x00` = connected, non-zero = dial failed.
fn serve_forward(mut stream: TcpStream, app: App, rt: tokio::runtime::Handle) {
    // Bound the pre-splice handshake reads: the data plane is published on 0.0.0.0 with
    // no auth, so a peer that connects and never sends the header would otherwise park
    // this OS thread forever (unbounded thread-exhaustion DoS). 10 s is ample for a
    // header + status handshake. This is CLEARED before splicing (below) so long-lived
    // idle established tunnels are not subject to it.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let header = match read_forward_header(&mut stream) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("forward header read failed: {e}");
            return;
        }
    };
    let host = app.store.get().hosts.into_iter().find(|h| h.id == header.host_id);
    let Some(host) = host else {
        let _ = stream.write_all(&[1u8]);
        return;
    };
    // Confine the data plane to *configured, enabled* forwards for this host — otherwise
    // the 0.0.0.0 listener is an open TCP proxy into the Docker network. The header must
    // name a forward that exists, is enabled, and targets the same remote port.
    let ok = host
        .forwards
        .iter()
        .any(|f| f.enabled && f.id == header.id && f.remote_port == header.remote_port);
    if !ok {
        let _ = stream.write_all(&[1u8]);
        return;
    }
    let dial = rt.block_on(app.dial_host(&host));
    let addr = format!("{dial}:{}", header.remote_port);
    match TcpStream::connect(&addr) {
        Ok(upstream) => {
            let _ = upstream.set_nodelay(true);
            if stream.write_all(&[0u8]).is_err() {
                return;
            }
            // Handshake complete: clear the read timeout BEFORE splicing so an idle
            // established tunnel is not torn down at 10 s. `splice_forward` makes its
            // `try_clone`s from `stream` below, so clearing here means the cloned fds
            // inherit no read timeout.
            let _ = stream.set_read_timeout(None);
            app.forwards.conn_opened(&header.host_id, &header.id);
            splice_forward(stream, upstream);
            app.forwards.conn_closed(&header.host_id, &header.id);
        }
        Err(e) => {
            tracing::info!("forward dial {addr} failed: {e}");
            let _ = stream.write_all(&[1u8]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connect a SOCK_SEQPACKET client to `path` — the daemon side of an accept pair
    /// (mirrors `clone-daemon`'s `Transport::connect`). The returned fd only has to stay
    /// alive; the test never sends on it.
    fn seq_connect(path: &str) -> OwnedFd {
        use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, connect, socket};
        let fd =
            socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None).unwrap();
        connect(fd.as_raw_fd(), &UnixAddr::new(path).unwrap()).unwrap();
        fd
    }

    /// The disconnect-teardown guard: a LATE old-thread teardown (its `recv()` erroring
    /// only after a replacement session already re-Hello'd under the same id) must leave
    /// the new session's `conns`/`latest` entries intact; the current session's own
    /// teardown still clears both maps.
    #[test]
    fn teardown_only_removes_own_connection() {
        // Two real accepted connections, as two serve_clone threads would hold them:
        // A = the old session (about to tear down late), B = the replacement.
        let path = std::env::temp_dir().join(format!("rmng-mp-test-{}.sock", std::process::id()));
        let path = path.to_str().unwrap().to_string();
        let listener = Listener::bind(&path).unwrap();
        let _client_a = seq_connect(&path);
        let conn_a = Arc::new(listener.accept().unwrap());
        let _client_b = seq_connect(&path);
        let conn_b = Arc::new(listener.accept().unwrap());
        let _ = std::fs::remove_file(&path);

        let conns: Mutex<HashMap<String, Arc<Conn>>> = Mutex::new(HashMap::new());
        let latest: Mutex<HashMap<String, HashMap<u32, LatestFrame>>> =
            Mutex::new(HashMap::new());
        // B has re-Hello'd: both maps hold the NEW session's state under id "x".
        conns.lock().unwrap().insert("x".into(), conn_b.clone());
        latest.lock().unwrap().insert("x".into(), HashMap::new());

        // Old thread A tears down late — B's entries must survive in BOTH maps.
        teardown_if_current(&conns, &latest, "x", &conn_a);
        assert!(
            conns.lock().unwrap().get("x").is_some_and(|c| Arc::ptr_eq(c, &conn_b)),
            "old-thread teardown clobbered the new session's conns entry"
        );
        assert!(
            latest.lock().unwrap().contains_key("x"),
            "old-thread teardown clobbered the new session's latest entry"
        );

        // The current session (B) tearing down removes both entries as before.
        teardown_if_current(&conns, &latest, "x", &conn_b);
        assert!(!conns.lock().unwrap().contains_key("x"));
        assert!(!latest.lock().unwrap().contains_key("x"));
    }

    #[test]
    fn build_forwards_msg_unions_enabled_rules_only() {
        use wire::{ControlState, Host, PortForward};
        let mut a = Host { id: "a".into(), host: "a".into(), ..Default::default() };
        a.forwards = vec![
            PortForward { id: "f8080".into(), remote_port: 3000, local_port: 8080, enabled: true, label: None },
            PortForward { id: "f9".into(), remote_port: 9, local_port: 9, enabled: false, label: None },
        ];
        let mut b = Host { id: "b".into(), host: "b".into(), ..Default::default() };
        b.forwards = vec![
            PortForward { id: "f7000".into(), remote_port: 5000, local_port: 7000, enabled: true, label: None },
        ];
        let st = ControlState { hosts: vec![a, b], ..Default::default() };
        let msg = build_forwards_msg(&st, 9005);
        assert_eq!(msg.forward_port, 9005);
        // Only the two enabled rules, tagged with host id.
        assert_eq!(msg.rules.len(), 2);
        assert!(msg.rules.iter().any(|r| r.host_id == "a" && r.local_port == 8080 && r.remote_port == 3000));
        assert!(msg.rules.iter().any(|r| r.host_id == "b" && r.local_port == 7000));
        assert!(!msg.rules.iter().any(|r| r.local_port == 9)); // disabled excluded
    }

    #[test]
    fn forward_header_frames_round_trip() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};
        let hdr = wire::forward::ForwardHeader { token: None, host_id: "h".into(), id: "f22".into(), remote_port: 22 };
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            read_forward_header(&mut s).unwrap()
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let body = serde_json::to_vec(&hdr).unwrap();
        c.write_all(&(body.len() as u32).to_be_bytes()).unwrap();
        c.write_all(&body).unwrap();
        assert_eq!(h.join().unwrap(), hdr);
    }

    #[test]
    fn splice_forward_pipes_both_ways() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        // "upstream" = an echo server.
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_addr = echo.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = echo.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).unwrap();
            s.write_all(&buf[..n]).unwrap();
        });
        // A pair standing in for the viewer-facing client socket.
        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_addr = front.local_addr().unwrap();
        let mut client = TcpStream::connect(front_addr).unwrap();
        let (server_side, _) = front.accept().unwrap();
        let upstream = TcpStream::connect(echo_addr).unwrap();
        std::thread::spawn(move || splice_forward(server_side, upstream));
        client.write_all(b"ping").unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping");
    }

    #[test]
    fn broadcast_drops_only_the_full_viewer() {
        use std::sync::mpsc::sync_channel;

        // Viewer A: capacity 4, actively has room. Viewer B: capacity 1, pre-filled → full.
        let (tx_a, rx_a) = sync_channel::<Arc<[u8]>>(4);
        let (tx_b, _rx_b) = sync_channel::<Arc<[u8]>>(1);
        let filler: Arc<[u8]> = Arc::from(vec![0u8; 1]);
        tx_b.try_send(filler).unwrap(); // B is now full (nobody drains _rx_b)

        let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
        viewers.lock().unwrap().insert(1, ViewerConn { id: 1, tx: tx_a });
        viewers.lock().unwrap().insert(2, ViewerConn { id: 2, tx: tx_b });

        let msg: Arc<[u8]> = Arc::from(vec![7u8, 8, 9]);
        broadcast_bytes(&viewers, &msg);

        // A received the message and stayed; B was full and got removed.
        assert_eq!(&*rx_a.recv().unwrap(), &[7u8, 8, 9]);
        assert!(viewers.lock().unwrap().contains_key(&1), "fast viewer was wrongly dropped");
        assert!(!viewers.lock().unwrap().contains_key(&2), "full viewer was not dropped");
    }

    #[test]
    fn remove_viewer_is_idempotent() {
        use std::sync::mpsc::sync_channel;
        let (tx, _rx) = sync_channel::<Arc<[u8]>>(1);
        let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
        viewers.lock().unwrap().insert(5, ViewerConn { id: 5, tx });
        remove_viewer(&viewers, 5);
        remove_viewer(&viewers, 5); // second call must be a no-op, not a panic
        assert!(viewers.lock().unwrap().is_empty());
    }

    #[test]
    fn frame_video_layout_matches_wire() {
        let au = [0xAAu8, 0xBB, 0xCC];
        let f = frame_video(0x01020304, &au);
        // [tag=0][monitor_id be][len be][au]
        assert_eq!(f[0], T_VIDEO);
        assert_eq!(&f[1..5], &0x01020304u32.to_be_bytes());
        assert_eq!(&f[5..9], &(au.len() as u32).to_be_bytes());
        assert_eq!(&f[9..], &au);
    }

    #[test]
    fn frame_json_layout_matches_wire() {
        let f = frame_json(T_CURSOR, &serde_json::json!({"x": 1})).unwrap();
        let json = serde_json::to_vec(&serde_json::json!({"x": 1})).unwrap();
        assert_eq!(f[0], T_CURSOR);
        assert_eq!(&f[1..5], &(json.len() as u32).to_be_bytes());
        assert_eq!(&f[5..], &json[..]);
    }

    #[test]
    fn viewer_src_roundtrips_and_rejects_clone_ids() {
        let s = viewer_src(42);
        assert_eq!(viewer_src_id(&s), Some(42));
        assert_eq!(viewer_src_id("some-clone-host"), None);
        assert_eq!(viewer_src_id("\0viewer:notanum"), None);
    }

    #[test]
    fn clip_forget_source_drops_owned_offer_and_scrubs_pending() {
        use wire::socket::ClipboardOffer;
        let clip: Mutex<ClipState> = Mutex::new(ClipState::default());
        let owner = viewer_src(1);
        let other = viewer_src(2);
        {
            let mut c = clip.lock().unwrap();
            c.offer = Some((ClipboardOffer { serial: 1, mime_types: vec!["text/plain".into()] }, owner.clone()));
            // A pending request that the owner (viewer 1) and viewer 2 both wanted.
            c.pending.insert((1, "text/plain".into()), vec![owner.clone(), other.clone()]);
        }

        clip_forget_source(&clip, &owner);

        let c = clip.lock().unwrap();
        assert!(c.offer.is_none(), "offer owned by the leaving viewer should be dropped");
        // owner scrubbed from the requester list; viewer 2 remains.
        assert_eq!(c.pending.get(&(1, "text/plain".into())), Some(&vec![other.clone()]));
    }

    #[test]
    fn clip_forget_source_removes_empty_pending_entries() {
        let clip: Mutex<ClipState> = Mutex::new(ClipState::default());
        let only = viewer_src(9);
        clip.lock().unwrap().pending.insert((7, "image/png".into()), vec![only.clone()]);
        clip_forget_source(&clip, &only);
        assert!(clip.lock().unwrap().pending.is_empty(), "emptied requester list should be removed");
    }
}
