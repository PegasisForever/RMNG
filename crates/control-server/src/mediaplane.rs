//! The media plane. It keeps each clone's daemon connection (input relay) + latest dmabuf
//! frame in a shared [`MediaHandle`], encodes the selected clone's desktop, and fans the
//! result out to the browser viewers.
//!
//! - Clone socket (bind-mounted): daemons connect + `Hello{clone_id}`, then stream
//!   per-monitor dmabuf frames; frames feed the encoders only while their clone is
//!   `selected`. Each frame is acked (1-deep flow control).
//! - Viewers: each browser window (`crate::webviewer`, a WebSocket on the web port) joins
//!   the viewer set through [`Plane`] and gets the selected clone's H.264 for its monitor,
//!   **one encoder per monitor**, plus the control messages. Its input is relayed to that
//!   clone.
//! - Selection switch / viewer connect → force a fresh IDR on every encoder.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::broadcast::error::RecvError;

use media::{Conn, Encoder, Listener};
use wire::browser::{FromBrowser, ToBrowser, ViewContent, ViewMonitor, ViewSpec, VideoHeader};
use wire::socket::{
    Ack, ClipboardData, ClipboardMsg, ClipboardOffer, ClipboardRequest, CursorMeta, DaemonMsg,
    InputMsg, ServerMsg,
};

use crate::app::App;

/// One connected viewer: its id + the sender into its per-viewer writer thread.
/// The writer thread owns the socket; producers only ever `try_send` here, so a slow
/// viewer's socket never blocks the encode or metadata threads.
pub(crate) struct ViewerConn {
    id: u64,
    tx: SyncSender<Queued>,
    /// The one monitor whose video this viewer (a browser window) gets.
    monitor: u32,
}

/// One message in a viewer's queue, already in its WebSocket form. Built once per broadcast
/// and cloned (refcount) into each viewer's channel.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Queued {
    /// A binary video message ([`VideoHeader`] + access unit), only for `monitor_id`'s viewers.
    Video { monitor_id: u32, bytes: Arc<[u8]> },
    /// Any other binary message: clipboard bytes ([`wire::browser::encode_clipboard_data`]).
    Binary(Arc<[u8]>),
    /// A JSON [`ToBrowser`] text message.
    Text(Arc<str>),
}

impl Queued {
    /// The message for `msg`. Clipboard bytes go binary, not as base64 in JSON.
    pub(crate) fn of(msg: &ToBrowser) -> Queued {
        if let ToBrowser::Clipboard(ClipboardMsg::Data(d)) = msg {
            return Queued::Binary(wire::browser::encode_clipboard_data(d).into());
        }
        Queued::Text(serde_json::to_string(msg).expect("ToBrowser serializes").into())
    }
}

/// The live set of viewers, keyed by id. Held under a short-lived lock only to
/// snapshot/insert/remove; all sends are non-blocking `try_send`.
pub(crate) type Viewers = Arc<Mutex<HashMap<u64, ViewerConn>>>;

/// Bounded per-viewer queue depth. `try_send` overflow disconnects the viewer (it
/// reconnects + re-primes). Bounds worst-case queued memory at CAP × max-AU per slow
/// viewer; tune here if joins on very bursty desktops need more slack.
pub(crate) const VIEWER_CHAN_CAP: usize = 128;

/// Monotonic per-process viewer id.
fn next_viewer_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Fan one message out to every viewer (video only to the viewers of its monitor). A viewer
/// whose channel is full (too slow) or disconnected (writer already gone) is removed from the
/// registry; dropping its stored `tx` lets its writer thread's channel disconnect, which tears
/// the viewer down. Never blocks: `try_send` is non-blocking.
fn broadcast_queued(viewers: &Viewers, msg: &Queued) {
    let mut guard = viewers.lock().unwrap();
    let mut dead: Vec<u64> = Vec::new();
    for v in guard.values() {
        if let Queued::Video { monitor_id, .. } = msg {
            if *monitor_id != v.monitor {
                continue;
            }
        }
        if v.tx.try_send(msg.clone()).is_err() {
            dead.push(v.id);
        }
    }
    for id in dead {
        // NB: if this viewer's writer thread is blocked in a send (not a full channel —
        // that's what got it removed here), dropping `tx` doesn't wake it. The writer's send
        // timeout ends it (the send timeout in `webviewer`).
        guard.remove(&id);
    }
}

/// Fan one control message out to every viewer.
pub(crate) fn broadcast(viewers: &Viewers, msg: &ToBrowser) {
    broadcast_queued(viewers, &Queued::of(msg));
}

/// Send one message to a single viewer; remove it on failure.
fn send_to(viewers: &Viewers, id: u64, msg: Queued) {
    let mut guard = viewers.lock().unwrap();
    if let Some(v) = guard.get(&id) {
        if v.tx.try_send(msg).is_err() {
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
    fd: OwnedFd,
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    /// Real per-plane (offset, stride) of the dmabuf, as reported by the daemon.
    planes: Vec<wire::socket::PlaneLayout>,
    /// `fd` is a memfd of system memory, not a dmabuf: this clone has no GPU.
    shm: bool,
}

impl LatestFrame {
    /// A copy with its own dup of the fd, so the cache keeps its own.
    fn dup(&self) -> Option<LatestFrame> {
        Some(LatestFrame {
            fd: dup_owned(&self.fd)?,
            planes: self.planes.clone(),
            ..*self
        })
    }

    /// Encode this frame (consumes the fd).
    fn push_to(self, enc: &Encoder) -> Result<(), String> {
        enc.push(
            self.fd,
            self.fourcc,
            self.modifier,
            self.width,
            self.height,
            &self.planes,
            self.shm,
        )
        .map_err(|e| e.to_string())
    }
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
    /// Wakeup for [`MediaHandle::is_connected`] waiters, notified on every Hello insert.
    /// Waiters always re-check `is_connected`, so a cross-clone wakeup is just a recheck
    /// and a Hello that lands before the wait starts is seen by the check, never missed.
    hello_notify: tokio::sync::Notify,
    /// clone id → (monitor_id → its latest dmabuf frame). Per-monitor so every monitor
    /// of a multi-monitor clone can be primed on viewer connect (not just the last one).
    latest: Mutex<HashMap<String, HashMap<u32, LatestFrame>>>,
    /// Rich/lazy clipboard broker state.
    clip: Mutex<ClipState>,
    /// clone id → (monitor_id → most-recent cursor *with a shape* on that monitor) — replayed
    /// to a viewer that connects mid-session (shapes are otherwise only sent on change). Per
    /// monitor: each monitor's capture stream reports its own cursor, and a window shows only
    /// its own monitor's.
    cursor: Mutex<HashMap<String, HashMap<u32, CursorMeta>>>,
    /// What we last told each clone about whether anyone is watching it, so a selection
    /// change sends one message to the two clones it concerns instead of to the fleet.
    capture_gate: Mutex<HashMap<String, bool>>,
    /// Set once by [`spawn`]; the browser viewer's WebSocket joins through it. Unset when
    /// media init failed, and then there is no video to watch.
    pub(crate) plane: OnceLock<Arc<Plane>>,
}

/// What a viewer joins: the viewer set, the encoders and the terminal plane.
pub(crate) struct Plane {
    viewers: Viewers,
    encoders: Encoders,
    termplane: Arc<crate::termplane::TermPlane>,
}

/// The clone whose desktop is on screen right now, or `None` when nobody is looking.
///
/// Both halves matter. Only the selected clone's frames are encoded, and with no viewer
/// connected not even that one is, so anything else a clone captures is thrown away.
fn watched_clone(app: &App, viewers: &Viewers) -> Option<String> {
    if viewers.lock().unwrap().is_empty() {
        return None;
    }
    app.store.selected()
}

/// Tell every connected clone-daemon whether a viewer is watching it, skipping the ones
/// already in the right state.
///
/// Capture is what makes a clone's compositor paint, so this is the difference between a
/// fleet where one desktop renders and one where all of them do. Call it whenever the answer
/// can change: a viewer arriving or leaving, and the selection moving.
fn update_capture_gates(handle: &MediaHandle, app: &App, viewers: &Viewers) {
    let watched = watched_clone(app, viewers);
    let conns: Vec<(String, Arc<Conn>)> = handle
        .conns
        .lock()
        .unwrap()
        .iter()
        .map(|(id, c)| (id.clone(), c.clone()))
        .collect();
    for (id, conn) in conns {
        let active = watched.as_deref() == Some(id.as_str());
        if handle.capture_gate.lock().unwrap().get(&id) == Some(&active) {
            continue;
        }
        match conn.send(&ServerMsg::Capture { active }) {
            Ok(()) => {
                handle
                    .capture_gate
                    .lock()
                    .unwrap()
                    .insert(id.clone(), active);
                tracing::debug!(
                    "clone '{id}': capture {}",
                    if active { "on" } else { "off" }
                );
            }
            // A clone that cannot take the message keeps capturing, which is safe: it is
            // exactly what every daemon did before this existed. Leave the record unset so
            // the next change tries again.
            Err(e) => tracing::warn!("clone '{id}': capture gate not delivered ({e:?})"),
        }
    }
}

/// Send a freshly-connected daemon its gate, whatever we last recorded for that clone.
///
/// Unconditional, unlike [`update_capture_gates`]: a daemon starts out capturing and a
/// restarted one has forgotten anything it was told, so "no change since last time" is not
/// the same as "it knows".
fn send_capture_gate_on_hello(handle: &MediaHandle, app: &App, viewers: &Viewers, id: &str) {
    let active = watched_clone(app, viewers).as_deref() == Some(id);
    let conn = handle.conns.lock().unwrap().get(id).cloned();
    let Some(conn) = conn else { return };
    if conn.send(&ServerMsg::Capture { active }).is_ok() {
        handle
            .capture_gate
            .lock()
            .unwrap()
            .insert(id.to_string(), active);
    }
}

fn dup_owned(fd: &OwnedFd) -> Option<OwnedFd> {
    let raw = nix::unistd::dup(fd.as_raw_fd()).ok()?;
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}

impl MediaHandle {
    /// Forward one input event to a clone-daemon.
    ///
    /// A full receive queue is not an error here. It means the clone has stopped reading —
    /// a paused one never will — and a pointer event is worthless by the time the queue
    /// drains anyway. Dropping it keeps the viewer's input thread moving, which matters
    /// because that thread is the only thing that tears the viewer down when it leaves.
    pub fn send_input(&self, clone: &str, input: InputMsg) -> Result<(), String> {
        let conn = self.conns.lock().unwrap().get(clone).cloned();
        match conn {
            Some(c) => match c.send(&ServerMsg::Input(input)) {
                Ok(()) | Err(media::sock::SendError::WouldBlock) => Ok(()),
                Err(e) => Err(e.to_string()),
            },
            None => Err(format!("clone '{clone}' not connected")),
        }
    }

    /// Push a live layout to one clone-daemon. Cheap: `Conn::send` is a single
    /// `MSG_DONTWAIT` `sendmsg`, and the clone's session holder ignores a layout equal to
    /// the one it already holds, so a repeat moves no windows.
    pub fn set_monitors(&self, clone: &str, monitors: &[wire::MonitorSpec]) -> Result<(), String> {
        let conn = self.conns.lock().unwrap().get(clone).cloned();
        match conn {
            Some(c) => c
                .send(&ServerMsg::SetMonitors {
                    monitors: monitors.to_vec(),
                })
                .map_err(|e| e.to_string()),
            None => Err(format!("clone '{clone}' not connected")),
        }
    }
    // NB: on-demand screenshots moved into the clone-daemon's own MCP (the fleet MCP
    // proxies to it); the control-server no longer encodes screenshots itself.

    /// Register a connection under `id`, as `Hello` does. Test-only: it lets a test stand in
    /// for a connected clone-daemon and read what the server pushed to it.
    #[cfg(test)]
    pub(crate) fn insert_conn_for_test(&self, id: &str, conn: Arc<Conn>) {
        self.conns.lock().unwrap().insert(id.to_string(), conn);
    }

    /// True when a clone-daemon session for `id` is live — i.e. its daemon has sent a
    /// `Hello{clone_id}` (keyed by clone_id == the clone's hostname == its host id) and
    /// the connection hasn't dropped. This is the readiness signal `provision.rs`'s
    /// clone-container wait-ready poll watches: a clone is "up + registered" once its
    /// daemon appears here.
    pub fn is_connected(&self, id: &str) -> bool {
        self.conns.lock().unwrap().contains_key(id)
    }

    /// Sleep until the next Hello from any clone or `fallback` elapses, whichever comes
    /// first (see `hello_notify`). The clone wait-ready loop uses this instead of a
    /// blind sleep so Hello usually lands within ms.
    pub(crate) async fn wait_hello_tick(&self, fallback: std::time::Duration) {
        tokio::select! {
            _ = self.hello_notify.notified() => {}
            _ = tokio::time::sleep(fallback) => {}
        }
    }
}

/// monitor_id → its H.264 encoder (lazily created for the selected clone).
/// monitor_id → (encoder, width, height). The size is tracked so a resolution change
/// (a clone reprovisioned/restarted with different `RMNG_MONITORS`) rebuilds the
/// encoder pipeline fresh — mid-stream VA-encoder resolution renegotiation is unreliable.
type Encoders = Arc<Mutex<HashMap<u32, (Arc<Encoder>, u32, u32)>>>;

/// Initialize GStreamer once. Must run **before** any
/// background task that can spawn a child process — inherited scanner pipes
/// otherwise hang init forever (ports never bind).
///
/// Failure is non-fatal: it returns false (call no [`spawn`]) so the web API still boots.
pub fn init() -> bool {
    match gstreamer::init() {
        Ok(()) => true,
        Err(e) => {
            tracing::error!("media init failed; no video for the viewer: {e}");
            false
        }
    }
}

/// Start the media plane. Call only after [`init`] returned true.
pub fn spawn(app: App) {
    // `spawn` is called from the async `main`, so a tokio runtime exists here; capture its
    // handle for the terminal plane's async docker calls.
    let rt_handle = tokio::runtime::Handle::current();
    let sock_path = wire::CLONE_SOCKET.to_string();
    let handle = app.media.clone();
    let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
    let encoders: Encoders = Arc::new(Mutex::new(HashMap::new()));
    let last_sel: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // Headless-clone terminal plane: proxies tmux sessions to viewers when the selected clone
    // is headless. Threaded into the connect path, the selection watcher, and each viewer's
    // message handler.
    let termplane = Arc::new(crate::termplane::TermPlane::new(
        app.clone(),
        viewers.clone(),
        rt_handle.clone(),
    ));

    let plane = Arc::new(Plane {
        viewers: viewers.clone(),
        encoders: encoders.clone(),
        termplane: termplane.clone(),
    });
    let _ = handle.plane.set(plane.clone());

    // Repaint the viewer the instant the *selected* clone changes, without waiting for the
    // next incoming frame. `/api/activate` only mutates the store; serve_clone's frame loop
    // otherwise notices the switch lazily — on the next frame from *any* clone — which on a
    // static, damage-driven desktop can be seconds away (the reported "few seconds before the
    // new clone appears"). Watch the store's change bus and, the moment `selected` flips, force
    // a fresh IDR on every encoder + re-prime from the newly-selected clone's last frame /
    // cursor / layout. Shares `last_sel` with the frame loop (same mutex, same lock order
    // last_sel → encoders/viewer), so the two paths serialize and never double-prime.
    {
        let (app, encoders, viewers, last_sel, handle, termplane) = (
            app.clone(),
            encoders.clone(),
            viewers.clone(),
            last_sel.clone(),
            handle.clone(),
            termplane.clone(),
        );
        std::thread::spawn(move || {
            let (_seed, mut rx) = app.store.subscribe();
            // Track the configured window set so a layout-preset change re-broadcasts a fresh
            // ViewSpec even when the *selection* is unchanged (the window set follows the layout,
            // not the daemon's report). Seeded to the current layout so the first tick is quiet.
            let mut last_monitors = configured_monitors(&app.config());
            loop {
                match rx.blocking_recv() {
                    // Any state mutation (or a lag) → re-check selection + layout; act on a change.
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        let state = app.store.get();
                        let sel = app.store.selected();
                        let monitors = configured_monitors(&app.config());
                        let monitors_changed = monitors != last_monitors;
                        last_monitors = monitors;
                        let sel_changed = adopt_selection(
                            &handle,
                            &encoders,
                            &viewers,
                            &app,
                            &termplane,
                            &last_sel,
                            sel.clone(),
                            "the store watcher",
                        );
                        if sel_changed || !monitors_changed {
                            continue;
                        }
                        // No viewers attached → nothing to repaint; the connect path primes on connect.
                        if viewers.lock().unwrap().is_empty() {
                            continue;
                        }
                        // Only the configured layout changed: refresh the window set in place.
                        // Headless clones re-broadcast their Terminal ViewSpec (with the new
                        // monitors) via termplane. Headed clones get a fresh Desktop ViewSpec.
                        match sel.as_deref() {
                            Some(s) if is_headless(&state, s) => termplane.on_viewers_changed(),
                            Some(_) => broadcast(
                                &viewers,
                                &ToBrowser::ViewSpec(desktop_view_spec(&app.config())),
                            ),
                            None => {}
                        }
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
                    let (handle, app, encoders, viewers, last_sel, termplane) = (
                        handle.clone(),
                        app.clone(),
                        encoders.clone(),
                        viewers.clone(),
                        last_sel.clone(),
                        termplane.clone(),
                    );
                    std::thread::spawn(move || {
                        serve_clone(conn, handle, app, encoders, viewers, last_sel, termplane)
                    });
                }
                Err(e) => {
                    tracing::error!("clone accept failed: {e}");
                    break;
                }
            }
        }
    });
}

/// The binary video message for one AU: [`VideoHeader`], then the Annex-B bytes.
fn video_message(monitor_id: u32, au: &[u8], key: bool, seq: u32) -> Queued {
    let server_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let header = VideoHeader {
        monitor_id,
        seq,
        key,
        server_us,
    };
    let mut bytes = Vec::with_capacity(wire::browser::VIDEO_HEADER_LEN + au.len());
    bytes.extend_from_slice(&header.encode());
    bytes.extend_from_slice(au);
    Queued::Video {
        monitor_id,
        bytes: bytes.into(),
    }
}

/// Send a clipboard message to one endpoint (a clone by id, or a viewer by src id).
fn send_clip_to(handle: &MediaHandle, viewers: &Viewers, dest: &str, msg: ClipboardMsg) {
    if let Some(id) = viewer_src_id(dest) {
        send_to(viewers, id, Queued::of(&ToBrowser::Clipboard(msg)));
        return;
    }
    let conn = handle.conns.lock().unwrap().get(dest).cloned();
    if let Some(c) = conn {
        let server_msg = match msg {
            ClipboardMsg::Offer(o) => ServerMsg::ClipboardOffer(o),
            ClipboardMsg::Request(r) => ServerMsg::ClipboardRequest(r),
            ClipboardMsg::Data(d) => ServerMsg::ClipboardData(d),
        };
        // Said out loud rather than dropped. A clipboard send that fails is a paste that
        // hangs in the clone until the daemon's own timeout fires, and the whole reason a
        // pasted image took forever and arrived corrupt was this error going unlogged.
        if let Err(e) = c.send(&server_msg) {
            tracing::warn!(target: "clip", "sending clipboard to {dest:?}: {e}");
        }
    }
}

/// Every clipboard destination except `source`: all *other* clones + all *other*
/// viewers (remote↔local + remote↔remote + viewer↔viewer).
fn clip_dests(handle: &MediaHandle, viewers: &Viewers, source: &str) -> Vec<String> {
    let mut dests: Vec<String> = handle
        .conns
        .lock()
        .unwrap()
        .keys()
        .filter(|&id| id != source)
        .cloned()
        .collect();
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

/// Record `requester` as wanting `req`, and say whether the owner has to be asked.
/// `Some(owner)` starts a fetch; `None` means either there is nothing to fetch or an
/// identical fetch is already in flight and this requester joins its reply.
///
/// Joining matters because one copy fans out to every clone and every viewer, and each
/// asks for the same `(serial, mime)` at once. The clone-daemon can only run one
/// `SelectionRead` at a time, so N identical requests would be N-1 failures.
fn clip_fetch_owner(
    clip: &Mutex<ClipState>,
    req: &ClipboardRequest,
    requester: &str,
) -> Option<String> {
    let mut clip = clip.lock().unwrap();
    let (_, owner) = clip.offer.clone()?;
    if owner == requester {
        return None; // don't ask the owner to fetch from itself
    }
    let key = (req.serial, req.mime_type.clone());
    let in_flight = clip.pending.contains_key(&key);
    clip.pending
        .entry(key)
        .or_default()
        .push(requester.to_string());
    (!in_flight).then_some(owner)
}

/// Broker: `requester` wants the current owner's bytes for a MIME — record it and
/// forward the request to the owner (lazy fetch), unless that fetch is already running.
fn broker_request(handle: &MediaHandle, viewers: &Viewers, req: ClipboardRequest, requester: &str) {
    let Some(owner) = clip_fetch_owner(&handle.clip, &req, requester) else {
        tracing::debug!(target: "clip", "request serial={} mime={} from {requester:?} not forwarded (no offer, self, or already in flight)", req.serial, req.mime_type);
        return;
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
    // Snapshot the handles, then release the lock before touching GStreamer: `force_idr`
    // pushes an event into the pipeline and can block on its streaming thread. Holding the
    // global map across that would stall every clone's reader thread (see `encoder_for`).
    let snapshot: Vec<Arc<Encoder>> = encoders
        .lock()
        .unwrap()
        .values()
        .map(|(e, _, _)| e.clone())
        .collect();
    for e in snapshot {
        e.force_idr();
    }
}

/// Get-or-create the encoder for a monitor at `w`×`h`; its AUs go to the viewers of
/// `monitor_id`. If an encoder exists at a different
/// resolution it is rebuilt fresh.
fn encoder_for(
    encoders: &Encoders,
    viewers: &Viewers,
    monitor_id: u32,
    w: u32,
    h: u32,
) -> Option<Arc<Encoder>> {
    // Fast path: an encoder at the right size already exists. Short critical section.
    // A stale-size entry is REMOVED here and shut down below, outside the lock:
    // dropping it in the map would destroy a PLAYING pipeline under the global
    // lock (`set_state(NULL)` blocks on the streaming threads) and stall every
    // clone's reader thread — the same rule `force_idr_all` documents.
    let stale: Option<Arc<Encoder>> = {
        let mut map = encoders.lock().unwrap();
        match map.get(&monitor_id) {
            Some((e, ew, eh)) if *ew == w && *eh == h => return Some(e.clone()),
            Some((_, ew, eh)) => {
                tracing::info!(
                    "monitor {monitor_id} resolution {ew}x{eh} → {w}x{h}; rebuilding encoder"
                );
                map.remove(&monitor_id).map(|(e, _, _)| e)
            }
            None => None,
        }
    };
    if let Some(old) = stale {
        old.shutdown();
    }
    // Build with the lock RELEASED. `Encoder::new` constructs a whole GStreamer pipeline and
    // blocks in `set_state(Playing)` (hundreds of ms, longer under GPU/CPU contention). This
    // map is global across clones, so building under it head-of-line-blocks every other
    // clone's reader thread — one slow encoder init froze the entire fleet's media sockets.
    let built = {
        let viewers = viewers.clone();
        let mut seq = 0u32;
        match Encoder::new(move |au, idr| {
            seq = seq.wrapping_add(1);
            broadcast_queued(&viewers, &video_message(monitor_id, &au, idr, seq))
        }) {
            Ok(e) => Arc::new(e),
            Err(err) => {
                tracing::error!("encoder for monitor {monitor_id} init failed: {err}");
                return None;
            }
        }
    };
    // Re-acquire and re-check: another thread may have built this monitor's encoder while we
    // were unlocked. First writer wins; our spare is dropped (cheap next to a fleet stall).
    let mut map = encoders.lock().unwrap();
    if let Some((e, ew, eh)) = map.get(&monitor_id) {
        if *ew == w && *eh == h {
            return Some(e.clone());
        }
    }
    map.insert(monitor_id, (built.clone(), w, h));
    Some(built)
}

/// The viewer's stable window set, from the server's **configured** monitor layout
/// (`AppConfig::effective_monitors`). Fleet-wide and always known — independent of any live
/// video — so the window set is identical across every clone. `id` is the layout slot index,
/// matching the daemon's `monitor_id` convention.
pub(crate) fn configured_monitors(cfg: &wire::AppConfig) -> Vec<ViewMonitor> {
    cfg.effective_monitors()
        .iter()
        .enumerate()
        .map(|(i, m)| ViewMonitor {
            id: i as u32,
            x: m.x as i32,
            y: m.y as i32,
            width: m.width,
            height: m.height,
            primary: m.primary,
        })
        .collect()
}

/// Bring one clone to the active layout preset.
///
/// A layout change reaches the fleet one clone at a time, on the switch to it. Applying it
/// everywhere at once rebuilds every clone's Mutter session in the same second, and the whole
/// board stutters for as long as that takes. A clone the operator is not watching keeps the
/// monitors it was last viewed with until they come back to it.
///
/// Best-effort and silent about a clone that is not connected: a headless clone runs no
/// daemon, and a booting one gets the layout from the `Hello` push instead. Cheap to repeat,
/// because the clone's session holder ignores a layout it already holds.
pub(crate) fn apply_active_layout(app: &App, clone: &str) {
    let monitors = app.config().effective_monitors();
    if let Err(e) = app.media.set_monitors(clone, &monitors) {
        tracing::debug!("no layout push to '{clone}': {e}");
    }
}

/// Whether a daemon's `Hello` should be answered with the active layout.
///
/// Two cases, and the second is the one that cost a fleet its monitors. A clone on screen has
/// to follow the active preset. A clone whose session holder this daemon just started has a
/// desktop seconds old, built on whatever the holder remembered, or on the built-in single
/// monitor when it remembered nothing. Nobody chose that layout and no window on it was
/// placed by hand, so it takes the active preset too.
///
/// Everything else is left alone. A daemon that restarted under a holder that kept running
/// still has the operator's windows where they left them, and that is exactly what the lazy
/// layout protects.
fn push_layout_on_hello(selected: Option<&str>, clone_id: &str, fresh_session: bool) -> bool {
    fresh_session || selected == Some(clone_id)
}

/// How long a new clone gets to register before it is left on the layout it booted with.
/// A first boot is slow (headless GNOME, the user units, the session holder's own startup),
/// and the create op returns before all of that finishes.
const NEW_CLONE_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// Bring a freshly created clone to the active layout preset as soon as its daemon registers.
///
/// A clone nobody has watched keeps the layout it boots with, and a brand-new one boots on the
/// single monitor baked into the template image. Nobody chose that layout, so the create path
/// calls this and the clone comes up on the preset the fleet is on, before anyone opens it.
/// The clone has no windows yet, so the swap costs nothing.
///
/// Polls rather than hooking `Hello`, because the daemon may register minutes after the create
/// op reports ready, or (a dead clone) never.
pub(crate) fn apply_active_layout_when_ready(app: App, clone: String) {
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + NEW_CLONE_WAIT;
        while tokio::time::Instant::now() < deadline {
            if app.media.is_connected(&clone) {
                apply_active_layout(&app, &clone);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        tracing::warn!(
            "clone '{clone}' never registered; it keeps the layout it booted with until it is \
             selected"
        );
    });
}

/// A connected clone-daemon for tests: the client fd, and the server side of the accept.
///
/// A `Conn` only exists on the far side of an accept, so a test that needs one binds a real
/// socket. `tag` keeps concurrent tests off each other's path. The client fd is what the test
/// reads to see what the server pushed.
#[cfg(test)]
pub(crate) fn accepted_conn(tag: &str) -> (OwnedFd, Arc<Conn>) {
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, connect, socket};
    let path = std::env::temp_dir().join(format!("rmng-conn-{}-{tag}.sock", std::process::id()));
    let path = path.to_str().unwrap().to_string();
    let listener = Listener::bind(&path).unwrap();
    let fd = socket(
        AddressFamily::Unix,
        SockType::SeqPacket,
        SockFlag::empty(),
        None,
    )
    .unwrap();
    connect(fd.as_raw_fd(), &UnixAddr::new(path.as_str()).unwrap()).unwrap();
    let conn = Arc::new(listener.accept().unwrap());
    let _ = std::fs::remove_file(&path);
    (fd, conn)
}

/// One datagram if the socket already holds one, `None` if it is empty. Test-only.
#[cfg(test)]
pub(crate) fn recv_now(fd: &OwnedFd) -> Option<Vec<u8>> {
    use nix::sys::socket::{MsgFlags, recv};
    let mut buf = [0u8; 8192];
    match recv(fd.as_raw_fd(), &mut buf, MsgFlags::MSG_DONTWAIT) {
        Ok(n) => Some(buf[..n].to_vec()),
        Err(_) => None,
    }
}

/// The `ViewSpec` for a selection: a `Desktop` view of the configured monitors for a headed
/// clone, an empty view when nothing is selected. Headless clones are **not** described here —
/// termplane owns their `Terminal` `ViewSpec` (it carries the live tmux session list).
fn desktop_view_spec(cfg: &wire::AppConfig) -> ViewSpec {
    ViewSpec {
        monitors: configured_monitors(cfg),
        content: ViewContent::Desktop,
    }
}

/// The `ViewSpec` with no monitors: nothing is selected.
fn empty_view_spec() -> ViewSpec {
    ViewSpec {
        monitors: vec![],
        content: ViewContent::Desktop,
    }
}

/// True if the selected clone id names a headless clone (its view is a terminal, owned by
/// termplane, not a Desktop `ViewSpec`).
fn is_headless(state: &wire::ControlState, sel: &str) -> bool {
    state.hosts.iter().any(|h| h.id == sel && h.headless)
}

/// Prime a freshly-connected viewer's metadata: push the selected clone's view spec / cursor /
/// clipboard offer straight into its own channel. Touches only this viewer's `tx` — no
/// encoder, no registry, so it's safe to call before the viewer is inserted into `viewers`. A
/// headless selection sends no `ViewSpec` here — termplane broadcasts the `Terminal` spec via
/// `on_viewers_changed` right after this viewer registers.
fn prime_viewer_metadata(
    handle: &MediaHandle,
    tx: &SyncSender<Queued>,
    selected: Option<String>,
    state: &wire::ControlState,
    cfg: &wire::AppConfig,
) {
    let send = |msg: ToBrowser| {
        let _ = tx.try_send(Queued::of(&msg));
    };
    let Some(sel) = selected else {
        // Nothing selected: an empty window set.
        send(ToBrowser::ViewSpec(empty_view_spec()));
        return;
    };
    // Headed clone → send the Desktop view spec now (headless is left to termplane).
    if !is_headless(state, &sel) {
        send(ToBrowser::ViewSpec(desktop_view_spec(cfg)));
    }
    // Last cursor shape + current clipboard offer, targeted to this viewer.
    for c in cached_cursors(handle, &sel) {
        send(ToBrowser::Cursor(c));
    }
    if let Some((offer, _)) = handle.clip.lock().unwrap().offer.clone() {
        send(ToBrowser::Clipboard(ClipboardMsg::Offer(offer)));
    }
}

/// The selected clone's last cursor with a shape, one per monitor.
fn cached_cursors(handle: &MediaHandle, sel: &str) -> Vec<CursorMeta> {
    let cursor = handle.cursor.lock().unwrap();
    cursor
        .get(sel)
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default()
}

/// Prime a viewer's video: force a shared keyframe and re-encode the selected clone's last
/// frame of `monitor` so it paints at once even on a static, damage-driven desktop. The
/// re-encode BROADCASTS via the shared encoder (existing
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
    monitor: u32,
) {
    let Some(sel) = selected else { return };
    // Re-encode the monitor's latest frame (broadcasts via the shared encoder). The `latest`
    // lock is released at the end of this statement, before any encode.
    let frame = handle
        .latest
        .lock()
        .unwrap()
        .get(&sel)
        .and_then(|m| m.get(&monitor))
        .and_then(LatestFrame::dup);
    let Some(f) = frame else { return };
    if let Some(enc) = encoder_for(encoders, viewers, monitor, f.width, f.height) {
        enc.force_idr();
        if let Err(e) = f.push_to(&enc) {
            tracing::warn!("prime re-encode failed: {e}");
        }
    }
}

/// Take `sel` as the selection everyone is being shown, and report whether it changed.
///
/// Two threads can be first to see a flip: the store watcher, and any clone's frame loop
/// (which re-reads `selected` per frame). `last_sel` decides which of them owns it, and the
/// winner has to do the WHOLE handover, because the loser then sees no change and does
/// nothing at all.
///
/// Half a handover is what leaves a switch frozen. The clone arriving on screen gets its
/// cached last frame painted (`reprime_all`) but is never told to capture, so no fresh frame
/// ever follows and the viewer sits on a picture from the last time anyone watched it. The
/// clone leaving keeps capturing, keeps shipping frames nobody encodes, and its frame loop is
/// the very thread most likely to steal the next flip too.
#[allow(clippy::too_many_arguments)]
fn adopt_selection(
    handle: &MediaHandle,
    encoders: &Encoders,
    viewers: &Viewers,
    app: &App,
    termplane: &crate::termplane::TermPlane,
    last_sel: &Mutex<Option<String>>,
    sel: Option<String>,
    claimant: &str,
) -> bool {
    let mut ls = last_sel.lock().unwrap();
    if *ls == sel {
        return false;
    }
    *ls = sel.clone();
    // Which thread got here first is the whole story when a switch misbehaves, and nothing
    // else in the log distinguishes them.
    tracing::debug!("selection → {sel:?}, claimed by {claimant}");
    // (De)activate the headless terminal view for the clone arriving on screen (handles
    // headless→headed, headed→headless, and headless→headless).
    termplane.on_viewers_changed();
    // Hand capture from the clone leaving the screen to the one arriving on it. Before the
    // viewer check below, because a selection change with no viewer attached still has to stop
    // the clone that was being watched a moment ago.
    update_capture_gates(handle, app, viewers);
    // No viewer attached → nothing to repaint; the connect path primes on connect.
    if !viewers.lock().unwrap().is_empty() {
        reprime_all(handle, encoders, viewers, app, sel);
    }
    true
}

/// Re-prime ALL viewers after a selection change: force fresh keyframes + rebroadcast
/// the newly-selected clone's view spec / cursor / last frame to everyone.
fn reprime_all(
    handle: &MediaHandle,
    encoders: &Encoders,
    viewers: &Viewers,
    app: &App,
    selected: Option<String>,
) {
    force_idr_all(encoders);
    let Some(sel) = selected else {
        // Nothing selected: clear every viewer's window set.
        broadcast(viewers, &ToBrowser::ViewSpec(empty_view_spec()));
        return;
    };
    // Headed clone → broadcast the Desktop view spec. Headless is owned by termplane: its
    // Terminal spec is broadcast from `on_viewers_changed` on the same selection flip.
    if !is_headless(&app.store.get(), &sel) {
        broadcast(viewers, &ToBrowser::ViewSpec(desktop_view_spec(&app.config())));
    }
    for c in cached_cursors(handle, &sel) {
        broadcast(viewers, &ToBrowser::Cursor(c));
    }
    // Copy the frames out first: the `latest` lock is never held across an encode.
    let frames: Vec<(u32, LatestFrame)> = handle
        .latest
        .lock()
        .unwrap()
        .get(&sel)
        .map(|m| m.iter().filter_map(|(mid, f)| Some((*mid, f.dup()?))).collect())
        .unwrap_or_default();
    for (mid, f) in frames {
        if let Some(enc) = encoder_for(encoders, viewers, mid, f.width, f.height) {
            f.push_to(&enc).ok();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn serve_clone(
    conn: Conn,
    handle: Arc<MediaHandle>,
    app: App,
    encoders: Encoders,
    viewers: Viewers,
    last_sel: Arc<Mutex<Option<String>>>,
    termplane: Arc<crate::termplane::TermPlane>,
) {
    let conn = Arc::new(conn);
    let mut clone_id: Option<String> = None;
    loop {
        match conn.recv() {
            Ok((DaemonMsg::Hello(h), _)) => {
                tracing::info!("clone-daemon '{}' connected", h.clone_id);
                handle
                    .conns
                    .lock()
                    .unwrap()
                    .insert(h.clone_id.clone(), conn.clone());
                handle.hello_notify.notify_waiters();
                // A daemon starts out capturing, so tell it at once if nobody is watching.
                send_capture_gate_on_hello(&handle, &app, &viewers, &h.clone_id);
                // Correct a stale layout on the clone that is on screen right now, and on one
                // whose session holder just came up. Every other clone catches up when the
                // operator switches to it (see `apply_active_layout`), so a fleet-wide daemon
                // restart does not rebuild every Mutter session at once.
                let selected = app.store.selected();
                if push_layout_on_hello(selected.as_deref(), &h.clone_id, h.fresh_session) {
                    let mons = app.config().effective_monitors();
                    if let Err(e) = conn.send(&ServerMsg::SetMonitors { monitors: mons }) {
                        tracing::warn!("SetMonitors on Hello for '{}' failed: {e}", h.clone_id);
                    }
                }
                clone_id = Some(h.clone_id);
            }
            Ok((DaemonMsg::Frame(f), fds)) => {
                let Some(id) = clone_id.clone() else { continue };
                if let Some(fd) = fds.into_iter().next() {
                    if let Some(dup) = dup_owned(&fd) {
                        handle
                            .latest
                            .lock()
                            .unwrap()
                            .entry(id.clone())
                            .or_default()
                            .insert(
                                f.monitor_id,
                                LatestFrame {
                                    fd: dup,
                                    fourcc: f.fourcc,
                                    modifier: f.modifier,
                                    width: f.width,
                                    height: f.height,
                                    planes: f.planes.clone(),
                                    shm: f.shm,
                                },
                            );
                    }
                    // Ack as soon as the frame is received and `latest` owns its own dup of the
                    // fd — BEFORE any encoding. The daemon's 1-deep gate exists precisely to shed
                    // frames when we cannot keep up, but it only clears on this Ack: acking after
                    // the encode inverts that contract. A slow encoder then stops the Acks, the
                    // daemon's gate never clears, its capture thread blocks forever in a blocking
                    // `sendmsg`, and the socket wedges — for EVERY clone, since this thread also
                    // stops draining. Observed live: all clone sockets frozen at a constant
                    // Recv-Q with the capture threads parked in `sock_alloc_send_pskb`.
                    // Acking here degrades to a lower frame rate under load instead of deadlock.
                    let _ = conn.send(&ServerMsg::Ack(Ack {
                        monitor_id: f.monitor_id,
                        seq: f.seq,
                    }));
                    let sel = app.store.selected();
                    // This loop re-reads the selection per frame, so it can beat the store
                    // watcher to a flip. When it does, it owns the whole handover.
                    adopt_selection(
                        &handle,
                        &encoders,
                        &viewers,
                        &app,
                        &termplane,
                        &last_sel,
                        sel.clone(),
                        &format!("clone '{id}'s frame loop"),
                    );
                    if sel.as_deref() == Some(id.as_str()) {
                        if let Some(enc) = encoder_for(
                            &encoders,
                            &viewers,
                            f.monitor_id,
                            f.width,
                            f.height,
                        ) {
                            if let Err(e) = enc.push(
                                fd, f.fourcc, f.modifier, f.width, f.height, &f.planes, f.shm,
                            ) {
                                tracing::warn!("encode push failed: {e}");
                            }
                        }
                    }
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
                        handle
                            .cursor
                            .lock()
                            .unwrap()
                            .entry(id.clone())
                            .or_default()
                            .insert(c.monitor_id, c.clone());
                    }
                    // Only the selected clone's cursor reaches the viewers.
                    if app.store.selected().as_deref() == Some(id.as_str()) {
                        broadcast(&viewers, &ToBrowser::Cursor(c));
                    }
                }
            }
            Ok((DaemonMsg::Layout { monitors: l }, _)) => {
                if let Some(id) = clone_id.clone() {
                    if app.store.selected().as_deref() == Some(id.as_str()) {
                        // Drop encoders for monitors that no longer exist on the selected
                        // clone (added/resized ones are (re)built lazily by encoder_for on
                        // the next frame). Prevents stale encoders lingering after a switch.
                        // Removed pipelines are shut down AFTER releasing the map: tearing
                        // down a PLAYING pipeline blocks on its streaming threads, and doing
                        // that under the global lock stalls every clone's reader thread.
                        // The viewer's window set comes from the configured layout (the
                        // ViewSpec), not this daemon-reported one, so nothing is sent here.
                        let live: std::collections::HashSet<u32> = l.iter().map(|m| m.id).collect();
                        let retired: Vec<Arc<Encoder>> = {
                            let mut encs = encoders.lock().unwrap();
                            let mut retired = Vec::new();
                            encs.retain(|mid, (e, _, _)| {
                                if live.contains(mid) {
                                    true
                                } else {
                                    retired.push(e.clone());
                                    false
                                }
                            });
                            retired
                        };
                        for e in retired {
                            e.shutdown();
                        }
                    }
                }
            }
            // Forward-compat: an unrecognized message from a newer daemon is ignored
            // rather than dropping the session (a genuine disconnect surfaces as `Err`).
            Ok((DaemonMsg::Unknown, _)) => {}
            Err(e) => {
                if let Some(id) = &clone_id {
                    teardown_if_current(
                        &handle.conns,
                        &handle.latest,
                        &handle.capture_gate,
                        id,
                        &conn,
                    );
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
/// nests these two locks (the frame path, [`prime_viewer_video`] and [`reprime_all`] take
/// `latest` alone and release it before they encode), so the conns→latest order cannot
/// invert.
fn teardown_if_current(
    conns: &Mutex<HashMap<String, Arc<Conn>>>,
    latest: &Mutex<HashMap<String, HashMap<u32, LatestFrame>>>,
    gates: &Mutex<HashMap<String, bool>>,
    id: &str,
    this: &Arc<Conn>,
) {
    let mut conns = conns.lock().unwrap();
    if conns.get(id).is_some_and(|c| Arc::ptr_eq(c, this)) {
        conns.remove(id);
        latest.lock().unwrap().remove(id);
        // Forget what this daemon was told. The next one to claim the id starts out
        // capturing and gets the current answer on its `Hello`.
        gates.lock().unwrap().remove(id);
    }
}

/// Add a viewer whose writer drains `tx`, and prime it: metadata first, then a fresh keyframe
/// of `monitor`, the one monitor whose video it gets. Returns its id. Blocks while an
/// encoder is built, so call it from a thread.
pub(crate) fn join_viewer(
    app: &App,
    plane: &Plane,
    tx: SyncSender<Queued>,
    monitor: u32,
) -> u64 {
    let handle = &app.media;
    let id = next_viewer_id();
    let state = app.store.get();
    let cfg = app.config();
    let selected = app.store.selected();
    // Queue the metadata straight into this viewer's channel.
    prime_viewer_metadata(handle, &tx, selected.clone(), &state, &cfg);
    // Register BEFORE the video re-encode so the keyframe reliably fans to this viewer
    // (independent of encode latency).
    plane
        .viewers
        .lock()
        .unwrap()
        .insert(id, ViewerConn { id, tx, monitor });
    // Somebody is watching now, so the selected clone has to start painting
    // again. Before the re-encode below, so its first fresh frame is close
    // behind the primed one.
    update_capture_gates(handle, app, &plane.viewers);
    // Only its own monitor, so it does not re-key the other windows.
    prime_viewer_video(handle, &plane.encoders, &plane.viewers, selected, monitor);
    // If the selected clone is headless, activate its tmux view and prime this
    // viewer with the current session list.
    plane.termplane.on_viewers_changed();
    id
}

/// Remove a viewer: drop it from the registry (stops fan-out, disconnects its writer), forget
/// its clipboard state, and let the terminal view and capture follow the new audience.
pub(crate) fn leave_viewer(app: &App, plane: &Plane, id: u64) {
    remove_viewer(&plane.viewers, id);
    clip_forget_source(&app.media.clip, &viewer_src(id));
    // A departing viewer may leave the headless terminal view with no audience → deactivate.
    plane.termplane.on_viewers_changed();
    // The last viewer leaving means no clone needs to paint at all.
    update_capture_gates(&app.media, app, &plane.viewers);
}

/// Act on one message from viewer `id`.
pub(crate) fn handle_viewer_msg(app: &App, plane: &Plane, id: u64, msg: FromBrowser) {
    let handle = &app.media;
    let viewers = &plane.viewers;
    match msg {
        FromBrowser::Input(input) => {
            if let Some(clone) = app.store.selected() {
                let _ = handle.send_input(&clone, input);
            }
        }
        FromBrowser::Clipboard(msg) => {
            let src = viewer_src(id);
            match msg {
                ClipboardMsg::Offer(o) => broker_offer(handle, viewers, o, &src),
                ClipboardMsg::Request(r) => broker_request(handle, viewers, r, &src),
                ClipboardMsg::Data(d) => broker_data(handle, viewers, d),
            }
        }
        FromBrowser::TermInput(msg) => plane.termplane.input(msg.session, msg.data),
        FromBrowser::TermResize(msg) => plane.termplane.resize(msg.cols, msg.rows),
        FromBrowser::TermNewSession => plane.termplane.new_session(),
        // Re-encode the monitor's last frame as a keyframe, as a join does: on a still
        // desktop no new frame may come for a long time.
        FromBrowser::RequestKeyframe(r) => prime_viewer_video(
            handle,
            &plane.encoders,
            viewers,
            app.store.selected(),
            r.monitor_id,
        ),
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
        let fd = socket(
            AddressFamily::Unix,
            SockType::SeqPacket,
            SockFlag::empty(),
            None,
        )
        .unwrap();
        connect(fd.as_raw_fd(), &UnixAddr::new(path).unwrap()).unwrap();
        fd
    }

    /// A layout push reaches the clone it names and no other. This is the whole point of the
    /// lazy layout: activating a preset used to rebuild every clone's Mutter session in the
    /// same second, which stalls the board. Now only the clone on screen swaps, and the rest
    /// take the layout when the operator switches to them.
    #[test]
    fn a_layout_push_reaches_only_the_named_clone() {
        let (client_a, conn_a) = accepted_conn("layout-a");
        let (client_b, conn_b) = accepted_conn("layout-b");
        let handle = MediaHandle::default();
        handle.insert_conn_for_test("a", conn_a);
        handle.insert_conn_for_test("b", conn_b);

        let mons = vec![wire::MonitorSpec {
            width: 1280,
            height: 720,
            x: 0,
            y: 0,
            primary: true,
        }];
        handle.set_monitors("a", &mons).unwrap();
        assert!(
            handle.set_monitors("c", &mons).is_err(),
            "no connection is not a push"
        );

        let got = recv_now(&client_a).expect("the named clone got the layout");
        match serde_json::from_slice::<ServerMsg>(&got).unwrap() {
            ServerMsg::SetMonitors { monitors } => assert_eq!(monitors, mons),
            other => panic!("expected SetMonitors, got {other:?}"),
        }
        assert!(
            recv_now(&client_b).is_none(),
            "the other clone keeps its own layout"
        );
    }

    /// The three `Hello` cases, and why the middle one exists: a fleet upgrading onto the
    /// session holder started one per clone, each holder came up on the built-in single
    /// monitor because it remembered nothing, and with only the selected clone being pushed to,
    /// every other clone stayed on a layout nobody chose.
    #[test]
    fn a_hello_takes_a_layout_when_it_is_watched_or_its_session_is_new() {
        // Watched: follows the active preset, as every other push does.
        assert!(push_layout_on_hello(Some("a"), "a", false));
        // Not watched, but this daemon just started the holder: the desktop behind it is
        // seconds old, so there is no window position to protect.
        assert!(push_layout_on_hello(Some("b"), "a", true));
        assert!(push_layout_on_hello(None, "a", true));
        // Not watched, holder still holding: the operator's windows are where they left them.
        assert!(!push_layout_on_hello(Some("b"), "a", false));
        assert!(!push_layout_on_hello(None, "a", false));
    }

    /// A clone created while the operator watches another one still comes up on the active
    /// preset. Nothing pushes to it before its daemon registers, and the push lands once it
    /// does, without anyone selecting it.
    #[tokio::test]
    async fn a_new_clone_takes_the_active_layout_once_its_daemon_registers() {
        let app = App::test_app();
        let (client, conn) = accepted_conn("new-clone");
        apply_active_layout_when_ready(app.clone(), "fresh".into());

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            recv_now(&client).is_none(),
            "pushed to a clone that had not registered"
        );

        app.media.insert_conn_for_test("fresh", conn);
        let mut got = None;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            got = recv_now(&client);
            if got.is_some() {
                break;
            }
        }
        let got = got.expect("the registered clone got the active layout");
        match serde_json::from_slice::<ServerMsg>(&got).unwrap() {
            ServerMsg::SetMonitors { monitors } => {
                assert_eq!(monitors, app.config().effective_monitors());
            }
            other => panic!("expected SetMonitors, got {other:?}"),
        }
    }

    /// Capture follows the operator's eyes: the selected clone is told to capture, every
    /// other connected clone is told to stop, and a clone already in the right state is not
    /// told anything (the gate map is what keeps a selection change off the whole fleet).
    #[tokio::test]
    async fn capture_gates_track_the_selection() {
        let app = App::test_app();
        let (client_a, conn_a) = accepted_conn("gate-a");
        let (client_b, conn_b) = accepted_conn("gate-b");
        app.media.insert_conn_for_test("a", conn_a);
        app.media.insert_conn_for_test("b", conn_b);
        let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));

        // No viewer connected: nobody captures, whatever the selection says.
        app.store.mutate(|s| s.selected = Some("a".into()));
        update_capture_gates(&app.media, &app, &viewers);
        assert_eq!(capture_msg(&client_a), Some(false));
        assert_eq!(capture_msg(&client_b), Some(false));

        // A viewer arrives on 'a': only 'a' starts, and 'b' hears nothing new.
        let (tx, _rx) = std::sync::mpsc::sync_channel::<Queued>(4);
        viewers.lock().unwrap().insert(
            1,
            ViewerConn {
                id: 1,
                tx,
                monitor: 0,
            },
        );
        update_capture_gates(&app.media, &app, &viewers);
        assert_eq!(capture_msg(&client_a), Some(true));
        assert_eq!(
            capture_msg(&client_b),
            None,
            "a clone already stopped was told again"
        );

        // Selection moves to 'b': exactly one stop and one start.
        app.store.mutate(|s| s.selected = Some("b".into()));
        update_capture_gates(&app.media, &app, &viewers);
        assert_eq!(capture_msg(&client_a), Some(false));
        assert_eq!(capture_msg(&client_b), Some(true));
    }

    /// Whoever claims a selection flip hands capture over, and only the first claimant acts.
    ///
    /// The regression this pins: a clone's frame loop used to claim the flip and then only
    /// repaint, leaving the arriving clone's capture gate shut. The operator saw that clone's
    /// last frame from the previous time anyone watched it, frozen, because no fresh frame
    /// could follow. The store watcher, seeing no change left to make, stayed silent.
    #[tokio::test]
    async fn whoever_sees_the_switch_first_hands_capture_over() {
        let app = App::test_app();
        let (client_a, conn_a) = accepted_conn("adopt-a");
        let (client_b, conn_b) = accepted_conn("adopt-b");
        app.media.insert_conn_for_test("a", conn_a);
        app.media.insert_conn_for_test("b", conn_b);
        let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
        let encoders: Encoders = Arc::new(Mutex::new(HashMap::new()));
        let last_sel: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let termplane = crate::termplane::TermPlane::new(
            app.clone(),
            viewers.clone(),
            tokio::runtime::Handle::current(),
        );
        let (tx, _rx) = std::sync::mpsc::sync_channel::<Queued>(64);
        viewers.lock().unwrap().insert(
            1,
            ViewerConn {
                id: 1,
                tx,
                monitor: 0,
            },
        );

        let adopt = |sel: Option<String>| {
            adopt_selection(
                &app.media,
                &encoders,
                &viewers,
                &app,
                &termplane,
                &last_sel,
                sel,
                "the test",
            )
        };

        app.store.mutate(|s| s.selected = Some("a".into()));
        assert!(adopt(Some("a".into())), "the first claimant owns the flip");
        assert_eq!(capture_msg(&client_a), Some(true));
        assert_eq!(capture_msg(&client_b), Some(false));

        // The switch: 'a' stops painting and 'b' starts, from one claim.
        app.store.mutate(|s| s.selected = Some("b".into()));
        assert!(adopt(Some("b".into())));
        assert_eq!(
            capture_msg(&client_a),
            Some(false),
            "the clone leaving kept capturing"
        );
        assert_eq!(
            capture_msg(&client_b),
            Some(true),
            "the clone arriving was never told to paint"
        );

        // The second observer of the same flip finds nothing to do and says nothing.
        assert!(
            !adopt(Some("b".into())),
            "the same selection was claimed twice"
        );
        assert_eq!(capture_msg(&client_a), None);
        assert_eq!(capture_msg(&client_b), None);
    }

    /// The frame loop must route its selection check through [`adopt_selection`]. Writing
    /// `last_sel` there directly claims the flip and silences the store watcher, which is
    /// exactly how the arriving clone ends up frozen on a stale frame. Guards the source,
    /// because reaching the real frame arm needs a live clone-daemon on a socket.
    #[test]
    fn the_frame_loop_claims_a_switch_through_the_shared_handover() {
        let src = include_str!("mediaplane.rs");
        let frame_arm = src
            .split_once("Ok((DaemonMsg::Frame(f), fds))")
            .expect("frame arm present")
            .1
            .split_once("Ok((DaemonMsg::ClipboardOffer")
            .expect("frame arm ends at the next match arm")
            .0;
        assert!(
            frame_arm.contains("adopt_selection("),
            "the frame arm must claim a selection change through adopt_selection"
        );
        assert!(
            !frame_arm.contains("last_sel.lock()"),
            "the frame arm claimed the flip itself, so the capture handover never runs"
        );
    }

    /// The `active` flag of the next `Capture` message on this socket, or `None` when the
    /// socket is empty. Panics on any other message, so a test cannot silently pass by
    /// reading something else.
    fn capture_msg(client: &OwnedFd) -> Option<bool> {
        let raw = recv_now(client)?;
        match serde_json::from_slice::<ServerMsg>(&raw).unwrap() {
            ServerMsg::Capture { active } => Some(active),
            other => panic!("expected Capture, got {other:?}"),
        }
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
        let latest: Mutex<HashMap<String, HashMap<u32, LatestFrame>>> = Mutex::new(HashMap::new());
        let gates: Mutex<HashMap<String, bool>> = Mutex::new(HashMap::new());
        // B has re-Hello'd: both maps hold the NEW session's state under id "x".
        conns.lock().unwrap().insert("x".into(), conn_b.clone());
        latest.lock().unwrap().insert("x".into(), HashMap::new());
        gates.lock().unwrap().insert("x".into(), true);

        // Old thread A tears down late — B's entries must survive in BOTH maps.
        teardown_if_current(&conns, &latest, &gates, "x", &conn_a);
        assert!(
            conns
                .lock()
                .unwrap()
                .get("x")
                .is_some_and(|c| Arc::ptr_eq(c, &conn_b)),
            "old-thread teardown clobbered the new session's conns entry"
        );
        assert!(
            latest.lock().unwrap().contains_key("x"),
            "old-thread teardown clobbered the new session's latest entry"
        );

        // The current session (B) tearing down removes both entries as before.
        teardown_if_current(&conns, &latest, &gates, "x", &conn_b);
        assert!(!conns.lock().unwrap().contains_key("x"));
        assert!(!latest.lock().unwrap().contains_key("x"));
    }

    /// The Ack must be emitted from the frame path BEFORE the encode step, not after it.
    /// The daemon's 1-deep gate only reopens on this Ack, so an Ack sequenced behind a slow
    /// (or wedged) encoder deadlocks the clone socket — and, because the same thread drains
    /// every clone, the whole fleet. Guards the source order rather than the runtime
    /// behaviour: `enc.push` needs a VA-API GPU pipeline that CI/test hosts do not have.
    #[test]
    fn frame_path_acks_before_encoding() {
        let src = include_str!("mediaplane.rs");
        let frame_arm = src
            .split_once("Ok((DaemonMsg::Frame(f), fds))")
            .expect("frame arm present")
            .1
            .split_once("Ok((DaemonMsg::ClipboardOffer")
            .expect("frame arm ends at the next match arm")
            .0;
        let ack = frame_arm
            .find("ServerMsg::Ack")
            .expect("frame arm sends an Ack");
        let push = frame_arm
            .find("enc.push(")
            .expect("frame arm pushes to an encoder");
        assert!(
            ack < push,
            "Ack must precede enc.push in the frame arm: acking after the encode makes the \
             daemon's flow-control gate depend on encoder progress and wedges the socket"
        );
    }

    #[test]
    fn broadcast_drops_only_the_full_viewer() {
        use std::sync::mpsc::sync_channel;

        // Viewer A: capacity 4, actively has room. Viewer B: capacity 1, pre-filled → full.
        let (tx_a, rx_a) = sync_channel::<Queued>(4);
        let (tx_b, _rx_b) = sync_channel::<Queued>(1);
        tx_b.try_send(Queued::Text("{}".into())).unwrap(); // B is now full (nobody drains _rx_b)

        let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
        viewers.lock().unwrap().insert(
            1,
            ViewerConn {
                id: 1,
                tx: tx_a,
                monitor: 0,
            },
        );
        viewers.lock().unwrap().insert(
            2,
            ViewerConn {
                id: 2,
                tx: tx_b,
                monitor: 0,
            },
        );

        broadcast_queued(&viewers, &Queued::Binary(Arc::from(vec![7u8, 8, 9])));

        // A received the message and stayed; B was full and got removed.
        assert_eq!(rx_a.recv().unwrap(), Queued::Binary(Arc::from(vec![7u8, 8, 9])));
        assert!(
            viewers.lock().unwrap().contains_key(&1),
            "fast viewer was wrongly dropped"
        );
        assert!(
            !viewers.lock().unwrap().contains_key(&2),
            "full viewer was not dropped"
        );
    }

    /// A browser window's queue gets only its own monitor's video, so the other monitors'
    /// frames do not fill it; everything else still reaches it.
    #[test]
    fn broadcast_video_reaches_only_its_monitors_viewer() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Queued>(4);
        let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
        viewers
            .lock()
            .unwrap()
            .insert(1, ViewerConn { id: 1, tx, monitor: 1 });
        broadcast_queued(&viewers, &video_message(0, &[0, 0, 0, 1, 0x65], true, 1));
        broadcast_queued(&viewers, &video_message(1, &[0, 0, 0, 1, 0x65], true, 2));
        broadcast(&viewers, &ToBrowser::ViewSpec(empty_view_spec()));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Queued::Video { monitor_id: 1, .. }
        ));
        assert!(matches!(rx.try_recv().unwrap(), Queued::Text(_)));
        assert!(rx.try_recv().is_err());
    }

    /// A video message is the browser header, then the access unit as it came.
    #[test]
    fn video_message_is_header_then_au() {
        let au = [0u8, 0, 0, 1, 0x65];
        let Queued::Video { monitor_id, bytes } = video_message(1, &au, true, 7) else {
            panic!("video is a video message");
        };
        assert_eq!(monitor_id, 1);
        let n = wire::browser::VIDEO_HEADER_LEN;
        assert_eq!(&bytes[..2], &[wire::browser::KIND_VIDEO, 1]);
        assert_eq!(&bytes[2..6], &1u32.to_be_bytes());
        assert_eq!(&bytes[6..10], &7u32.to_be_bytes());
        assert_eq!(&bytes[n..], &au);
    }

    /// Clipboard bytes go out as a binary message; every other message is `ToBrowser` JSON.
    #[test]
    fn clipboard_data_is_binary_and_the_rest_is_json() {
        let data = ClipboardData {
            serial: 5,
            mime_type: "image/png".into(),
            bytes: vec![1, 2, 3],
        };
        let Queued::Binary(b) = Queued::of(&ToBrowser::Clipboard(ClipboardMsg::Data(data.clone())))
        else {
            panic!("data is a binary message");
        };
        assert_eq!(wire::browser::decode_clipboard_data(&b), Some(data));
        let offer = ToBrowser::Clipboard(ClipboardMsg::Offer(ClipboardOffer {
            serial: 5,
            mime_types: vec!["image/png".into()],
        }));
        let Queued::Text(t) = Queued::of(&offer) else {
            panic!("an offer is a text message");
        };
        assert_eq!(serde_json::from_str::<ToBrowser>(&t).unwrap(), offer);
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
            c.offer = Some((
                ClipboardOffer {
                    serial: 1,
                    mime_types: vec!["text/plain".into()],
                },
                owner.clone(),
            ));
            // A pending request that the owner (viewer 1) and viewer 2 both wanted.
            c.pending
                .insert((1, "text/plain".into()), vec![owner.clone(), other.clone()]);
        }

        clip_forget_source(&clip, &owner);

        let c = clip.lock().unwrap();
        assert!(
            c.offer.is_none(),
            "offer owned by the leaving viewer should be dropped"
        );
        // owner scrubbed from the requester list; viewer 2 remains.
        assert_eq!(
            c.pending.get(&(1, "text/plain".into())),
            Some(&vec![other.clone()])
        );
        drop(c);
        // An emptied requester list is removed rather than left behind to grow
        // the map; the surviving entry for viewer 2 is untouched.
        let lone = viewer_src(9);
        clip.lock()
            .unwrap()
            .pending
            .insert((7, "image/png".into()), vec![lone.clone()]);
        clip_forget_source(&clip, &lone);
        let c = clip.lock().unwrap();
        assert!(c.pending.get(&(7, "image/png".into())).is_none());
        assert_eq!(c.pending.len(), 1);
    }

    #[test]
    fn a_second_requester_joins_the_running_fetch_instead_of_starting_another() {
        use wire::socket::{ClipboardOffer, ClipboardRequest};
        let clip: Mutex<ClipState> = Mutex::new(ClipState::default());
        clip.lock().unwrap().offer = Some((
            ClipboardOffer {
                serial: 4,
                mime_types: vec!["text/plain".into()],
            },
            "clone-a".into(),
        ));
        let req = ClipboardRequest {
            serial: 4,
            mime_type: "text/plain".into(),
        };

        let first = clip_fetch_owner(&clip, &req, &viewer_src(1));
        let second = clip_fetch_owner(&clip, &req, &viewer_src(2));

        assert_eq!(
            first.as_deref(),
            Some("clone-a"),
            "the first request starts the fetch"
        );
        assert_eq!(
            second, None,
            "the second must not start a parallel SelectionRead"
        );
        // Both still get the bytes: the reply fans out over the pending list.
        assert_eq!(
            clip.lock().unwrap().pending.get(&(4, "text/plain".into())),
            Some(&vec![viewer_src(1), viewer_src(2)])
        );
    }

    #[test]
    fn a_different_mime_or_serial_starts_its_own_fetch() {
        use wire::socket::{ClipboardOffer, ClipboardRequest};
        let clip: Mutex<ClipState> = Mutex::new(ClipState::default());
        clip.lock().unwrap().offer = Some((
            ClipboardOffer {
                serial: 4,
                mime_types: vec!["text/plain".into(), "text/html".into()],
            },
            "clone-a".into(),
        ));
        let text = ClipboardRequest {
            serial: 4,
            mime_type: "text/plain".into(),
        };
        let html = ClipboardRequest {
            serial: 4,
            mime_type: "text/html".into(),
        };
        let newer = ClipboardRequest {
            serial: 5,
            mime_type: "text/plain".into(),
        };

        assert!(clip_fetch_owner(&clip, &text, &viewer_src(1)).is_some());
        assert!(
            clip_fetch_owner(&clip, &html, &viewer_src(1)).is_some(),
            "html is a separate transfer"
        );
        assert!(
            clip_fetch_owner(&clip, &newer, &viewer_src(1)).is_some(),
            "a new offer is a separate transfer"
        );
    }

    #[test]
    fn the_owner_is_never_asked_to_fetch_from_itself() {
        use wire::socket::{ClipboardOffer, ClipboardRequest};
        let clip: Mutex<ClipState> = Mutex::new(ClipState::default());
        clip.lock().unwrap().offer = Some((
            ClipboardOffer {
                serial: 1,
                mime_types: vec!["text/plain".into()],
            },
            "clone-a".into(),
        ));
        let req = ClipboardRequest {
            serial: 1,
            mime_type: "text/plain".into(),
        };
        assert_eq!(clip_fetch_owner(&clip, &req, "clone-a"), None);
        assert!(
            clip.lock().unwrap().pending.is_empty(),
            "the owner must not be recorded as a requester"
        );
    }
}
