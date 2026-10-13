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

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};

use wire::browser::{FromBrowser, ToBrowser};

use crate::app::App;
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
        loop {
            let next = rt.block_on(async {
                tokio::select! {
                    m = tokio::time::timeout(READ_TIMEOUT, stream.next()) => m.ok().flatten(),
                    _ = &mut gone_rx => None,
                }
            });
            match next {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<FromBrowser>(&text) {
                    Ok(msg) => mediaplane::handle_viewer_msg(&app, &plane, id, msg),
                    Err(e) => tracing::debug!("browser viewer {id}: bad message: {e}"),
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
        mediaplane::leave_viewer(&app, &plane, id);
        tracing::info!("browser viewer {id} left");
        // Dropping `stream` here and `sink` in the writer closes the socket.
    });
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
        T_CLIPBOARD => ToBrowser::Clipboard(serde_json::from_slice(body).ok()?),
        T_CURSOR => ToBrowser::Cursor(serde_json::from_slice(body).ok()?),
        T_VIEWSPEC => ToBrowser::ViewSpec(serde_json::from_slice(body).ok()?),
        T_MODE => ToBrowser::Mode(serde_json::from_slice(body).ok()?),
        T_TERM_DATA => ToBrowser::TermData(serde_json::from_slice(body).ok()?),
        _ => return None,
    };
    Some(Message::Text(serde_json::to_string(&msg).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::browser::VideoHeader;

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
