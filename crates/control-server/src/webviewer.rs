//! The browser viewer's WebSocket (`/api/viewer?monitor=<id>&tab=<page id>`, protocol in
//! [`wire::browser`]).
//!
//! A browser viewer is one entry in the media plane's viewer set: it joins, is primed, gets the
//! broadcast through its queue ([`Outbox`]: control first, then video, then clipboard pieces),
//! and is closed when it makes no progress (the browser then reconnects and is primed again).
//!
//! - Writer thread: drains the queue (the media plane queues only this socket's monitor's
//!   video) and sends it.
//! - Reader thread: parses [`FromBrowser`], puts clipboard pieces together, and hands it to the
//!   media plane's handler. It owns the teardown.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use futures::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;

use std::collections::BTreeSet;
use std::path::Path;

use wire::browser::{FromBrowser, ToBrowser};
use wire::socket::{ClipboardData, ClipboardMsg, InputMsg, TouchMsg};

use crate::app::App;
use crate::clone_home::CloneHome;
use crate::mediaplane::{self, Queued};
use crate::outbox::{Next, Outbox};
use crate::shared::CLONE_UID;

/// The writer pings this often, so a browser that has gone away is noticed: Chrome answers
/// every ping with a pong, which the reader counts as traffic.
const PING_EVERY: Duration = Duration::from_secs(5);
/// No message at all (pongs and acknowledgements included) for this long closes the socket.
/// One write has no time limit; the socket is closed only when the browser stops
/// acknowledging what was sent ([`Outbox::stalled`]).
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Serve one browser viewer window showing `monitor`. `tab` is the page's id, the same on each
/// reconnect: a new socket of it replaces the old one ([`mediaplane::join_viewer`]).
pub(crate) fn serve(app: App, monitor: u32, tab: Option<String>, ws: WebSocket) {
    let Some(plane) = app.media.plane.get().cloned() else {
        return; // media init failed: no video plane to join
    };
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let (mut sink, mut stream) = ws.split();
        let outbox = Arc::new(Outbox::new());
        // Tells the reader the writer gave up, so it tears down without waiting for a read.
        let (gone_tx, mut gone_rx) = tokio::sync::oneshot::channel::<()>();

        let (writer_rt, writer_box) = (rt.clone(), outbox.clone());
        std::thread::spawn(move || {
            let mut last_ping = Instant::now();
            loop {
                let msg = if last_ping.elapsed() >= PING_EVERY {
                    last_ping = Instant::now();
                    Message::Ping(Vec::new())
                } else {
                    match writer_box.next(PING_EVERY.saturating_sub(last_ping.elapsed())) {
                        Next::Msg(q) => message(q),
                        Next::Idle => continue,
                        Next::Closed => break,
                    }
                };
                // A closed queue ends a send that waits for the socket.
                let sent = writer_rt.block_on(async {
                    tokio::select! {
                        r = sink.send(msg) => r.is_ok(),
                        _ = writer_box.closed() => false,
                    }
                });
                if !sent {
                    break;
                }
            }
            drop(gone_tx);
        });

        // First on the socket, so the page knows the build before anything else arrives.
        outbox.push(Queued::of(&ToBrowser::Version { build_id: app.build_id() }));
        let (mode, speed) = app.effective_scroll();
        outbox.push(Queued::of(&ToBrowser::Scroll { mode, speed }));
        let id = mediaplane::next_viewer_id();
        mediaplane::join_viewer(&app, &plane, id, outbox.clone(), monitor, tab);
        tracing::info!("browser viewer {id} connected (monitor {monitor})");
        // Input follows the selection: what this socket holds in the old clone is released at
        // its next input message (a dashboard switch blurs the window, which releases all) and
        // at close.
        let mut held = Held::default();
        let mut upload = Upload::default();
        let mut pieces_in = 0u32;
        let mut deadline = tokio::time::Instant::now() + READ_TIMEOUT;
        loop {
            let next = rt.block_on(async {
                tokio::select! {
                    m = tokio::time::timeout_at(deadline, stream.next()) => m.ok().flatten(),
                    _ = &mut gone_rx => None,
                }
            });
            deadline = tokio::time::Instant::now() + READ_TIMEOUT;
            // Checked on every message: a live browser sends an acknowledgement at least once a
            // second, and Chrome answers every ping.
            let now = Instant::now();
            if outbox.stalled(now) {
                tracing::info!("browser viewer {id}: no frame acknowledged for 10 s; closing");
                break;
            }
            if let Some(s) = outbox.summary(now) {
                tracing::debug!("browser viewer {id}: {s}");
            }
            let msg = match next {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<FromBrowser>(&text) {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::debug!("browser viewer {id}: bad message: {e}");
                        continue;
                    }
                },
                Some(Ok(Message::Binary(b))) => {
                    let Some((piece, last)) = wire::browser::decode_clipboard_piece(&b) else {
                        continue;
                    };
                    // The browser sends its next piece when it hears this one arrived.
                    pieces_in = pieces_in.wrapping_add(1);
                    outbox.push(Queued::of(&ToBrowser::ClipAck { n: pieces_in }));
                    match upload.add(piece, last) {
                        Some(d) => FromBrowser::Clipboard(ClipboardMsg::Data(d)),
                        None => continue,
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => continue,
            };
            if let FromBrowser::Input(input) = &msg {
                held.follow(&app, app.store.selected());
                held.track(input);
            }
            if let FromBrowser::Touch(touch) = &msg {
                held.follow(&app, app.store.selected());
                held.track_touch(touch);
            }
            // A newer copy: the browser stopped sending the old one.
            if let FromBrowser::Clipboard(ClipboardMsg::Offer(_)) = &msg {
                upload = Upload::default();
            }
            mediaplane::handle_viewer_msg(&app, &plane, id, msg);
        }
        // A window that closes with a key down (Ctrl+W: Chrome sends Ctrl's press, then
        // closes) never sends the release, so send it here.
        held.follow(&app, None);
        mediaplane::leave_viewer(&app, &plane, id, monitor);
        // Also when the registry dropped it already: stops the writer, also inside a send.
        outbox.close();
        tracing::info!("browser viewer {id} left");
        // Dropping `stream` here and `sink` in the writer closes the socket.
    });
}

/// The keys, buttons and touch slots this socket holds down in the clone that got them, so
/// they can be released in that clone when the selection moves to another one or the socket
/// ends.
#[derive(Default)]
struct Held {
    clone: Option<String>,
    keys: BTreeSet<u32>,
    buttons: BTreeSet<i32>,
    touches: BTreeSet<u32>,
}

impl Held {
    fn track(&mut self, msg: &InputMsg) {
        match msg {
            InputMsg::KeyCode { keycode, pressed } => {
                if *pressed {
                    self.keys.insert(*keycode);
                } else {
                    self.keys.remove(keycode);
                }
            }
            InputMsg::Button { button, pressed } => {
                if *pressed {
                    self.buttons.insert(*button);
                } else {
                    self.buttons.remove(button);
                }
            }
            _ => {}
        }
    }

    fn track_touch(&mut self, msg: &TouchMsg) {
        match *msg {
            TouchMsg::Down { slot, .. } => {
                self.touches.insert(slot);
            }
            TouchMsg::Up { slot } => {
                self.touches.remove(&slot);
            }
            TouchMsg::Motion { .. } => {}
        }
    }

    /// Input now goes to `selected`: if that is another clone, release everything held in
    /// the old one first.
    fn follow(&mut self, app: &App, selected: Option<String>) {
        if self.clone == selected {
            return;
        }
        if let Some(old) = std::mem::replace(&mut self.clone, selected) {
            for msg in self.releases() {
                let _ = app.media.send_input(&old, msg);
            }
            for slot in std::mem::take(&mut self.touches) {
                let _ = app.media.send_touch(&old, TouchMsg::Up { slot });
            }
        }
    }

    /// Release messages for everything held; afterwards nothing is held.
    fn releases(&mut self) -> Vec<InputMsg> {
        let keys = std::mem::take(&mut self.keys)
            .into_iter()
            .map(|keycode| InputMsg::KeyCode {
                keycode,
                pressed: false,
            });
        let buttons =
            std::mem::take(&mut self.buttons)
                .into_iter()
                .map(|button| InputMsg::Button {
                    button,
                    pressed: false,
                });
        keys.chain(buttons).collect()
    }
}

/// The WebSocket message for one queued message.
fn message(q: Queued) -> Message {
    match q {
        Queued::Video { bytes, .. } | Queued::Binary(bytes) | Queued::Audio(bytes) => {
            Message::Binary(bytes.to_vec())
        }
        Queued::Text(text) => Message::Text(text.to_string()),
        Queued::Clipboard(_) => unreachable!("the outbox sends a clipboard reply in pieces"),
    }
}

/// A clipboard reply from the browser, arriving in pieces
/// ([`wire::browser::encode_clipboard_piece`]).
#[derive(Default)]
struct Upload {
    reply: Option<ClipboardData>,
    /// The reply grew past the largest clipboard message; it ends as "could not read".
    too_big: bool,
}

impl Upload {
    /// Add one piece. A piece of another serial or MIME type starts a new reply (the browser
    /// stopped sending the old one). The whole reply once its last piece came.
    fn add(&mut self, piece: ClipboardData, last: bool) -> Option<ClipboardData> {
        match &mut self.reply {
            Some(r) if r.serial == piece.serial && r.mime_type == piece.mime_type => {
                if !self.too_big {
                    r.bytes.extend_from_slice(&piece.bytes);
                }
            }
            _ => (self.reply, self.too_big) = (Some(piece), false),
        }
        let r = self.reply.as_mut()?;
        if r.bytes.len() > wire::socket::chunk::MAX_MESSAGE_BYTES {
            (r.bytes, self.too_big) = (Vec::new(), true);
        }
        if last {
            self.too_big = false;
            self.reply.take()
        } else {
            None
        }
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct UploadQuery {
    name: String,
}

/// `POST /api/viewer/upload?name=<file name>`: a file dropped on a viewer window. It is written
/// into the selected clone's home (`~/`), owned by the clone user. The body is the raw file,
/// streamed to disk, so its size has no cap. A name that exists already gets ` (1)`, ` (2)`…
/// before its extension; the reply says the name used.
pub(crate) async fn upload(
    State(app): State<App>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let err = |code: StatusCode| move |e: std::io::Error| (code, e.to_string());
    let name = Path::new(&q.name)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or((StatusCode::BAD_REQUEST, format!("bad file name {:?}", q.name)))?;
    let id = app
        .store
        .selected()
        .ok_or((StatusCode::CONFLICT, "no clone is selected".to_string()))?;
    let home = CloneHome::of(&app, &id).merged();
    if !home.is_dir() {
        return Err((StatusCode::CONFLICT, format!("clone {id} has no home here")));
    }
    // Written beside the target and renamed at the end, so a cut-off upload never shows under
    // the file's name.
    let tmp = home.join(format!(".{name}.rmng-upload"));
    let written = async {
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            file.write_all(&chunk.map_err(std::io::Error::other)?).await?;
        }
        file.flush().await?;
        std::os::unix::fs::chown(&tmp, Some(CLONE_UID), Some(CLONE_UID))?;
        let used = free_name(name, |n| home.join(n).exists());
        tokio::fs::rename(&tmp, home.join(&used)).await?;
        Ok::<_, std::io::Error>(used)
    }
    .await;
    let used = match written {
        Ok(used) => used,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR)(e));
        }
    };
    tracing::info!("viewer upload: {used} into {id}'s home");
    Ok(Json(serde_json::json!({ "name": used })))
}

/// `name`, or the first of `stem (1).ext`, `stem (2).ext`… that does not exist.
fn free_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|c| !exists(c))
        .expect("some suffix is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket that ends with keys and buttons down releases exactly those.
    #[test]
    fn held_input_is_released() {
        let mut held = Held::default();
        let key = |keycode, pressed| InputMsg::KeyCode { keycode, pressed };
        let button = |button, pressed| InputMsg::Button { button, pressed };
        for m in [
            key(29, true),
            key(30, true),
            key(30, false),
            button(0x110, true),
            button(0x111, true),
            button(0x111, false),
        ] {
            held.track(&m);
        }
        assert_eq!(held.releases(), vec![key(29, false), button(0x110, false)]);
        assert!(held.releases().is_empty());
    }

    /// The touch slots still down are the ones lifted when the selection moves or the socket
    /// ends.
    #[test]
    fn held_touches_are_tracked_by_slot() {
        let mut held = Held::default();
        let down = |slot| TouchMsg::Down { monitor_id: 0, slot, x: 1.0, y: 2.0 };
        for m in [down(0), down(1), TouchMsg::Motion { monitor_id: 0, slot: 1, x: 3.0, y: 4.0 }, TouchMsg::Up { slot: 0 }] {
            held.track_touch(&m);
        }
        assert_eq!(held.touches.iter().copied().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn a_clipboard_reply_is_put_together_from_its_pieces() {
        let piece = |serial, bytes: &[u8]| ClipboardData {
            serial,
            mime_type: "image/png".into(),
            bytes: bytes.to_vec(),
        };
        let mut up = Upload::default();
        assert_eq!(up.add(piece(1, b"ab"), false), None);
        assert_eq!(up.add(piece(1, b"cd"), true), Some(piece(1, b"abcd")));
        // A piece of a newer copy drops what came of the older one.
        assert_eq!(up.add(piece(2, b"ab"), false), None);
        assert_eq!(up.add(piece(3, b"x"), false), None);
        assert_eq!(up.add(piece(3, b"y"), true), Some(piece(3, b"xy")));
        // A reply in one piece.
        assert_eq!(up.add(piece(4, b""), true), Some(piece(4, b"")));
    }

    #[test]
    fn a_taken_name_gets_a_suffix() {
        let taken = ["a.txt", "a (1).txt", "notes", ".bashrc"];
        let exists = |n: &str| taken.contains(&n);
        assert_eq!(free_name("b.txt", exists), "b.txt");
        assert_eq!(free_name("a.txt", exists), "a (2).txt");
        assert_eq!(free_name("notes", exists), "notes (1)");
        assert_eq!(free_name(".bashrc", exists), ".bashrc (1)");
    }
}
