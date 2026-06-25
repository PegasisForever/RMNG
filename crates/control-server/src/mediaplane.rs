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

use media::{Conn, Encoder, Listener};
use wire::socket::{
    Ack, ClipboardData, ClipboardMsg, ClipboardOffer, ClipboardRequest, CursorMeta, DaemonMsg,
    InputMsg, MonitorPlacement, ServerMsg,
};

use crate::app::App;

/// Central clipboard broker state (rich + lazy). One current `offer` (the selection
/// owner) + the in-flight `pending` requests so `Data` replies route back to whoever
/// asked. Source/requester ids are clone ids or [`VIEWER_SRC`].
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

/// Pseudo-source id for clipboard that originated at the (single) viewer.
const VIEWER_SRC: &str = "\0viewer";

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
}

type Viewer = Arc<Mutex<Option<TcpStream>>>;
/// monitor_id → its H.264 encoder (lazily created for the selected clone).
/// monitor_id → (encoder, width, height). The size is tracked so a resolution change
/// (a clone reprovisioned/restarted with different `RMNG_MONITORS`) rebuilds the
/// encoder pipeline fresh — mid-stream VA-encoder resolution renegotiation is unreliable.
type Encoders = Arc<Mutex<HashMap<u32, (Arc<Encoder>, u32, u32)>>>;

pub fn spawn(app: App) {
    if let Err(e) = media::init() {
        tracing::error!("media init failed; port 1 disabled: {e}");
        return;
    }
    let cfg = app.config();
    let video_port = cfg.listen.video;
    let sock_path =
        std::env::var("RMNG_CLONE_SOCKET").unwrap_or_else(|_| "/srv/rmng-sock/clones.sock".into());
    let handle = app.media.clone();
    let viewer: Viewer = Arc::new(Mutex::new(None));
    let encoders: Encoders = Arc::new(Mutex::new(HashMap::new()));
    let last_sel: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Port 1 TCP: viewers + their input.
    {
        let (viewer, handle, app, encoders) = (viewer.clone(), handle.clone(), app.clone(), encoders.clone());
        std::thread::spawn(move || match TcpListener::bind(("0.0.0.0", video_port)) {
            Ok(l) => {
                tracing::info!("port 1 (video) listening on 0.0.0.0:{video_port}");
                for stream in l.incoming().flatten() {
                    let _ = stream.set_nodelay(true);
                    tracing::info!("viewer connected: {:?}", stream.peer_addr());
                    if let Ok(input_sock) = stream.try_clone() {
                        *viewer.lock().unwrap() = Some(stream);
                        force_idr_all(&encoders); // fresh keyframes so the viewer paints at once
                        // Prime the new viewer with the selected clone's last-known state so
                        // it paints immediately even on a static desktop (METADATA capture is
                        // damage-driven → no fresh frame until something changes).
                        prime_viewer(&handle, &encoders, &viewer, app.store.selected());
                        // Advertise the current clipboard offer so the new viewer can
                        // expose it to local apps (bytes fetched lazily on paste).
                        if let Some((offer, _)) = handle.clip.lock().unwrap().offer.clone() {
                            write_clip(&viewer, &ClipboardMsg::Offer(offer));
                        }
                        let (handle, app, viewer2) = (handle.clone(), app.clone(), viewer.clone());
                        std::thread::spawn(move || read_viewer_input(input_sock, handle, app, viewer2));
                    }
                }
            }
            Err(e) => tracing::error!("port 1 bind {video_port} failed: {e}"),
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
                    let (handle, app, encoders, viewer, last_sel) =
                        (handle.clone(), app.clone(), encoders.clone(), viewer.clone(), last_sel.clone());
                    std::thread::spawn(move || serve_clone(conn, handle, app, encoders, viewer, last_sel));
                }
                Err(e) => {
                    tracing::error!("clone accept failed: {e}");
                    break;
                }
            }
        }
    });
}

/// Port-1 frame types (1-byte tag prefix). 0 = video, 1 = clipboard, 2 = cursor.
const T_VIDEO: u8 = 0;
const T_CLIPBOARD: u8 = 1;
const T_CURSOR: u8 = 2;
const T_LAYOUT: u8 = 3;

/// Frame one H.264 AU to the viewer: `[0u8][u32be monitor_id][u32be len][AnnexB]`.
fn write_frame(viewer: &Viewer, monitor_id: u32, au: &[u8]) {
    let mut guard = viewer.lock().unwrap();
    if let Some(sock) = guard.as_mut() {
        let ok = sock
            .write_all(&[T_VIDEO])
            .and_then(|_| sock.write_all(&monitor_id.to_be_bytes()))
            .and_then(|_| sock.write_all(&(au.len() as u32).to_be_bytes()))
            .and_then(|_| sock.write_all(au));
        if ok.is_err() {
            *guard = None;
        }
    }
}

/// Push a clipboard message to the viewer: `[1u8][u32be len][JSON ClipboardMsg]`.
fn write_clip(viewer: &Viewer, msg: &ClipboardMsg) {
    write_json_frame(viewer, T_CLIPBOARD, msg);
}

/// Push a cursor update to the viewer: `[2u8][u32be len][JSON CursorMeta]`.
fn write_layout(viewer: &Viewer, layout: &[MonitorPlacement]) {
    write_json_frame(viewer, T_LAYOUT, &layout);
}

fn write_cursor(viewer: &Viewer, cursor: &CursorMeta) {
    write_json_frame(viewer, T_CURSOR, cursor);
}

/// Frame a tagged JSON message to the viewer: `[tag][u32be len][json]`.
fn write_json_frame<T: serde::Serialize>(viewer: &Viewer, tag: u8, msg: &T) {
    let Ok(json) = serde_json::to_vec(msg) else { return };
    let mut guard = viewer.lock().unwrap();
    if let Some(sock) = guard.as_mut() {
        let ok = sock
            .write_all(&[tag])
            .and_then(|_| sock.write_all(&(json.len() as u32).to_be_bytes()))
            .and_then(|_| sock.write_all(&json));
        if ok.is_err() {
            *guard = None;
        }
    }
}

/// Send a clipboard message to one endpoint (a clone by id, or the viewer).
fn send_clip_to(handle: &MediaHandle, viewer: &Viewer, dest: &str, msg: ClipboardMsg) {
    if dest == VIEWER_SRC {
        write_clip(viewer, &msg);
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

/// Broker: a new selection is offered by `source`. Becomes the current clipboard;
/// advertise it to the viewer + every *other* clone (remote↔local + remote↔remote).
fn broker_offer(handle: &MediaHandle, viewer: &Viewer, offer: ClipboardOffer, source: &str) {
    handle.clip.lock().unwrap().offer = Some((offer.clone(), source.to_string()));
    let dests: Vec<String> = clip_dests(handle, source);
    for d in dests {
        send_clip_to(handle, viewer, &d, ClipboardMsg::Offer(offer.clone()));
    }
}

/// Broker: `requester` wants the current owner's bytes for a MIME — record it and
/// forward the request to the owner (lazy fetch).
fn broker_request(handle: &MediaHandle, viewer: &Viewer, req: ClipboardRequest, requester: &str) {
    let owner = {
        let mut clip = handle.clip.lock().unwrap();
        let Some((_, owner)) = clip.offer.clone() else { return };
        if owner == requester {
            return; // don't ask the owner to fetch from itself
        }
        clip.pending.entry((req.serial, req.mime_type.clone())).or_default().push(requester.to_string());
        owner
    };
    send_clip_to(handle, viewer, &owner, ClipboardMsg::Request(req));
}

/// Broker: the owner returned bytes — deliver to everyone who requested them.
fn broker_data(handle: &MediaHandle, viewer: &Viewer, data: ClipboardData) {
    let requesters = handle
        .clip
        .lock()
        .unwrap()
        .pending
        .remove(&(data.serial, data.mime_type.clone()))
        .unwrap_or_default();
    for r in requesters {
        send_clip_to(handle, viewer, &r, ClipboardMsg::Data(data.clone()));
    }
}

/// Every clipboard destination except `source`: the viewer + all other clones.
fn clip_dests(handle: &MediaHandle, source: &str) -> Vec<String> {
    let mut dests: Vec<String> = handle.conns.lock().unwrap().keys().cloned().filter(|id| id != source).collect();
    if source != VIEWER_SRC {
        dests.push(VIEWER_SRC.to_string());
    }
    dests
}

fn force_idr_all(encoders: &Encoders) {
    for (e, _, _) in encoders.lock().unwrap().values() {
        e.force_idr();
    }
}

/// Prime a freshly-connected viewer with the selected clone's last-known state:
/// re-encode the last captured frame (so a static, damage-driven METADATA desktop
/// still paints at once) and replay the last cursor shape (otherwise only sent on
/// change). No-op if nothing is selected / captured yet.
fn prime_viewer(handle: &MediaHandle, encoders: &Encoders, viewer: &Viewer, selected: Option<String>) {
    let Some(sel) = selected else { return };
    // Video: re-encode each monitor's latest frame for an immediate keyframe, so every
    // monitor of a static, damage-driven METADATA desktop paints at once (not just one).
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
        if let Some(enc) = encoder_for(encoders, viewer, mid, w, h) {
            enc.force_idr();
            if let Err(e) = enc.push(fd, fourcc, modifier, w, h) {
                tracing::warn!("prime re-encode failed: {e}");
            }
        }
    }
    // Layout: replay so the viewer can place its windows + route drags from the start.
    if let Some(l) = handle.layout.lock().unwrap().get(&sel).cloned() {
        write_layout(viewer, &l);
    }
    // Cursor: replay the last shape so the client can draw it before it next moves.
    if let Some(c) = handle.cursor.lock().unwrap().get(&sel).cloned() {
        write_cursor(viewer, &c);
    }
}

/// Get-or-create the encoder for a monitor at `w`×`h`; its AUs are framed with
/// `monitor_id`. If an encoder exists at a different resolution it is rebuilt fresh.
fn encoder_for(encoders: &Encoders, viewer: &Viewer, monitor_id: u32, w: u32, h: u32) -> Option<Arc<Encoder>> {
    let mut map = encoders.lock().unwrap();
    if let Some((e, ew, eh)) = map.get(&monitor_id) {
        if *ew == w && *eh == h {
            return Some(e.clone());
        }
        tracing::info!("monitor {monitor_id} resolution {ew}x{eh} → {w}x{h}; rebuilding encoder");
    }
    let viewer = viewer.clone();
    match Encoder::new(move |au, _idr| write_frame(&viewer, monitor_id, &au)) {
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

fn serve_clone(
    conn: Conn,
    handle: Arc<MediaHandle>,
    app: App,
    encoders: Encoders,
    viewer: Viewer,
    last_sel: Arc<Mutex<Option<String>>>,
) {
    let conn = Arc::new(conn);
    let mut clone_id: Option<String> = None;
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
                            force_idr_all(&encoders);
                            // Repaint from the newly-selected clone's last frame + cursor.
                            prime_viewer(&handle, &encoders, &viewer, sel.clone());
                        }
                    }
                    if sel.as_deref() == Some(id.as_str()) {
                        if let Some(enc) = encoder_for(&encoders, &viewer, f.monitor_id, f.width, f.height) {
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
                    tracing::debug!("clipboard offer from clone '{id}': {} mime(s)", o.mime_types.len());
                    broker_offer(&handle, &viewer, o, &id);
                }
            }
            Ok((DaemonMsg::ClipboardRequest(r), _)) => {
                if let Some(id) = clone_id.clone() {
                    broker_request(&handle, &viewer, r, &id);
                }
            }
            Ok((DaemonMsg::ClipboardData(d), _)) => {
                broker_data(&handle, &viewer, d);
            }
            Ok((DaemonMsg::Cursor(c), _)) => {
                if let Some(id) = clone_id.clone() {
                    // Remember the last cursor *with a shape* to replay on viewer connect.
                    if c.shape.is_some() {
                        handle.cursor.lock().unwrap().insert(id.clone(), c.clone());
                    }
                    // Only the selected clone's cursor reaches the viewer.
                    if app.store.selected().as_deref() == Some(id.as_str()) {
                        write_cursor(&viewer, &c);
                    }
                }
            }
            Ok((DaemonMsg::Layout { monitors: l }, _)) => {
                if let Some(id) = clone_id.clone() {
                    handle.layout.lock().unwrap().insert(id.clone(), l.clone());
                    if app.store.selected().as_deref() == Some(id.as_str()) {
                        write_layout(&viewer, &l);
                    }
                }
            }
            Err(e) => {
                if let Some(id) = &clone_id {
                    handle.conns.lock().unwrap().remove(id);
                    handle.latest.lock().unwrap().remove(id);
                    tracing::info!("clone-daemon '{id}' disconnected: {e}");
                }
                break;
            }
        }
    }
}

/// Viewer → server: `[u8 type][u32be len][JSON]`. type 0 = InputMsg (to the selected
/// clone), type 1 = ClipboardData (broker fans it out to the clones).
fn read_viewer_input(mut sock: TcpStream, handle: Arc<MediaHandle>, app: App, viewer: Viewer) {
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
                let Some(id) = app.store.selected() else { continue };
                let _ = handle.send_input(&id, input);
            }
            T_CLIPBOARD => {
                if let Ok(msg) = serde_json::from_slice::<ClipboardMsg>(&body) {
                    match msg {
                        ClipboardMsg::Offer(o) => broker_offer(&handle, &viewer, o, VIEWER_SRC),
                        ClipboardMsg::Request(r) => broker_request(&handle, &viewer, r, VIEWER_SRC),
                        ClipboardMsg::Data(d) => broker_data(&handle, &viewer, d),
                    }
                }
            }
            _ => break,
        }
    }
}
