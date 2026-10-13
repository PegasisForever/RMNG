//! The browser viewer's WebSocket (`/api/viewer?monitor=<id>`, protocol in [`wire::browser`]).
//!
//! A browser viewer is one more entry in the media plane's viewer set, next to the port-1
//! viewers: it joins, is primed, gets the same broadcast, and is closed by the same rule when
//! its 128-message queue fills (the browser then reconnects and is primed again). Only the
//! wire format differs, so this file is the translation and the two per-socket threads.
//!
//! - Writer thread: drains the queue (the media plane queues only this socket's monitor's
//!   video) and turns the port-1 framing into a binary video message or a JSON [`ToBrowser`]
//!   text message.
//! - Reader thread: parses [`FromBrowser`] and hands it to the media plane's handler. It owns
//!   the teardown, as on port 1.

use std::sync::mpsc::RecvTimeoutError;
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
use wire::socket::{ClipboardMsg, InputMsg};

use crate::app::App;
use crate::clone_home::CloneHome;
use crate::mediaplane::{
    self, Queued, T_CLIPBOARD, T_CURSOR, T_MODE, T_TERM_DATA, T_VIDEO, T_VIEWSPEC,
};

/// A send that makes no progress for this long closes the socket (the browser stopped reading).
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// The writer pings this often, so a browser that has gone away is noticed: Chrome answers
/// every ping with a pong, which the reader counts as traffic.
const PING_EVERY: Duration = Duration::from_secs(5);
/// No message at all (pongs included) for this long closes the socket.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Serve one browser viewer window showing `monitor`.
pub(crate) fn serve(app: App, monitor: u32, ws: WebSocket) {
    let Some(plane) = app.media.plane.get().cloned() else {
        return; // media init failed: no video plane to join
    };
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let (mut sink, mut stream) = ws.split();
        let (tx, rx) = std::sync::mpsc::sync_channel::<Queued>(mediaplane::VIEWER_CHAN_CAP);
        // Tells the reader the writer gave up, so it tears down without waiting for a read.
        let (gone_tx, mut gone_rx) = tokio::sync::oneshot::channel::<()>();

        let writer_rt = rt.clone();
        std::thread::spawn(move || {
            let mut last_ping = Instant::now();
            loop {
                let msg = if last_ping.elapsed() >= PING_EVERY {
                    last_ping = Instant::now();
                    Message::Ping(Vec::new())
                } else {
                    match rx.recv_timeout(PING_EVERY.saturating_sub(last_ping.elapsed())) {
                        Ok(q) => match browser_message(&q) {
                            Some(m) => m,
                            None => continue,
                        },
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                };
                // Built inside `block_on`: a tokio timer needs the runtime when it is created.
                let sent = writer_rt
                    .block_on(async { tokio::time::timeout(SEND_TIMEOUT, sink.send(msg)).await });
                if !matches!(sent, Ok(Ok(()))) {
                    break;
                }
            }
            drop(gone_tx);
        });

        let id = mediaplane::join_viewer(&app, &plane, tx, Some(monitor));
        tracing::info!("browser viewer {id} connected (monitor {monitor})");
        let mut held = Held::default();
        loop {
            let next = rt.block_on(async {
                tokio::select! {
                    m = tokio::time::timeout(READ_TIMEOUT, stream.next()) => m.ok().flatten(),
                    _ = &mut gone_rx => None,
                }
            });
            let msg = match next {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<FromBrowser>(&text) {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::debug!("browser viewer {id}: bad message: {e}");
                        continue;
                    }
                },
                Some(Ok(Message::Binary(b))) => match wire::browser::decode_clipboard_data(&b) {
                    Some(d) => FromBrowser::Clipboard(ClipboardMsg::Data(d)),
                    None => continue,
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => continue,
            };
            held.track(&msg);
            mediaplane::handle_viewer_msg(&app, &plane, id, msg);
        }
        // A window that closes with a key down (Ctrl+W: Chrome sends Ctrl's press, then
        // closes) never sends the release, so send it here.
        for msg in held.releases() {
            mediaplane::handle_viewer_msg(&app, &plane, id, msg);
        }
        mediaplane::leave_viewer(&app, &plane, id);
        tracing::info!("browser viewer {id} left");
        // Dropping `stream` here and `sink` in the writer closes the socket.
    });
}

/// The keys and buttons this socket holds down in the clone, so they can be released when it
/// ends.
#[derive(Default)]
struct Held {
    keys: BTreeSet<u32>,
    buttons: BTreeSet<i32>,
}

impl Held {
    fn track(&mut self, msg: &FromBrowser) {
        match msg {
            FromBrowser::Input(InputMsg::KeyCode { keycode, pressed }) => {
                if *pressed {
                    self.keys.insert(*keycode);
                } else {
                    self.keys.remove(keycode);
                }
            }
            FromBrowser::Input(InputMsg::Button { button, pressed }) => {
                if *pressed {
                    self.buttons.insert(*button);
                } else {
                    self.buttons.remove(button);
                }
            }
            _ => {}
        }
    }

    fn releases(self) -> impl Iterator<Item = FromBrowser> {
        let keys = self.keys.into_iter().map(|keycode| InputMsg::KeyCode {
            keycode,
            pressed: false,
        });
        let buttons = self.buttons.into_iter().map(|button| InputMsg::Button {
            button,
            pressed: false,
        });
        keys.chain(buttons).map(FromBrowser::Input)
    }
}

/// Turn one queued port-1 message into this socket's message, or `None` for the forward set
/// (a browser cannot open local ports).
fn browser_message(q: &Queued) -> Option<Message> {
    let buf = &q.buf;
    if buf[0] == T_VIDEO {
        let header = q.video?;
        let au = &buf[9..];
        let mut out = Vec::with_capacity(wire::browser::VIDEO_HEADER_LEN + au.len());
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(au);
        return Some(Message::Binary(out));
    }
    let body = &buf[5..];
    let msg = match buf[0] {
        T_CLIPBOARD => match serde_json::from_slice(body).ok()? {
            // The bytes go raw, not as base64 inside JSON.
            ClipboardMsg::Data(d) => {
                return Some(Message::Binary(wire::browser::encode_clipboard_data(&d)));
            }
            m => ToBrowser::Clipboard(m),
        },
        T_CURSOR => ToBrowser::Cursor(serde_json::from_slice(body).ok()?),
        T_VIEWSPEC => ToBrowser::ViewSpec(serde_json::from_slice(body).ok()?),
        T_MODE => ToBrowser::Mode(serde_json::from_slice(body).ok()?),
        T_TERM_DATA => ToBrowser::TermData(serde_json::from_slice(body).ok()?),
        _ => return None,
    };
    Some(Message::Text(serde_json::to_string(&msg).ok()?))
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

/// The clone user (`rmng`), who owns everything in a clone's home.
const CLONE_UID: u32 = 1000;

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
    use wire::browser::VideoHeader;

    /// A socket that ends with keys and buttons down releases exactly those.
    #[test]
    fn held_input_is_released() {
        let mut held = Held::default();
        let key = |keycode, pressed| FromBrowser::Input(InputMsg::KeyCode { keycode, pressed });
        let button = |button, pressed| FromBrowser::Input(InputMsg::Button { button, pressed });
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
        assert_eq!(
            held.releases().collect::<Vec<_>>(),
            vec![key(29, false), button(0x110, false)]
        );
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

    /// Clipboard bytes go out as a binary message; an offer stays JSON.
    #[test]
    fn clipboard_data_is_binary() {
        let data = wire::socket::ClipboardData {
            serial: 5,
            mime_type: "image/png".into(),
            bytes: vec![1, 2, 3],
        };
        let q = mediaplane::frame_json(T_CLIPBOARD, &ClipboardMsg::Data(data.clone())).unwrap();
        let Some(Message::Binary(b)) = browser_message(&q.into()) else {
            panic!("data is a binary message");
        };
        assert_eq!(wire::browser::decode_clipboard_data(&b), Some(data));
        let offer = ClipboardMsg::Offer(wire::socket::ClipboardOffer {
            serial: 5,
            mime_types: vec!["image/png".into()],
        });
        let q = mediaplane::frame_json(T_CLIPBOARD, &offer).unwrap();
        assert!(matches!(browser_message(&q.into()), Some(Message::Text(_))));
    }

    /// Video keeps its monitor and gets the browser header; JSON messages keep their body and
    /// gain the `t` tag; the forward set does not reach a browser.
    #[test]
    fn port1_messages_translate_for_the_browser() {
        let header = VideoHeader {
            monitor_id: 1,
            seq: 7,
            key: true,
            server_us: 42,
        };
        let video = Queued {
            buf: mediaplane::frame_video(1, &[0, 0, 0, 1, 0x65]),
            video: Some(header),
        };
        let Some(Message::Binary(b)) = browser_message(&video) else {
            panic!("video is a binary message");
        };
        assert_eq!(&b[..18], &header.encode());
        assert_eq!(&b[18..], &[0, 0, 0, 1, 0x65]);

        let mode = mediaplane::frame_json(
            T_MODE,
            &wire::viewer::ModeMsg {
                chroma: wire::ChromaMode::Yuv420,
            },
        )
        .unwrap();
        let Some(Message::Text(t)) = browser_message(&mode.into()) else {
            panic!("mode is a text message");
        };
        assert_eq!(
            serde_json::from_str::<ToBrowser>(&t).unwrap(),
            ToBrowser::Mode(wire::viewer::ModeMsg {
                chroma: wire::ChromaMode::Yuv420
            })
        );

        let forwards =
            mediaplane::frame_json(mediaplane::T_FORWARDS, &serde_json::json!({})).unwrap();
        assert!(browser_message(&forwards.into()).is_none());
    }
}
