//! Connection-scoped outgoing queue. UI/input threads enqueue; only the worker writes to TCP.
//!
//! Motion coalesces only across adjacent compatible events, never across a click/key/scroll
//! or a monitor change. Input can pass queued clipboard data, but cannot interrupt a frame
//! already being written. Clipboard messages retain their order relative to one another.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Condvar, Mutex};

use wire::socket::InputMsg;

// Bound both tiny-event floods and large clipboard transfers. One additional message can
// be in flight outside the queue. Reserve space for input even with a full bulk queue.
const MAX_MESSAGES: usize = 1024;
const MAX_BYTES: usize = 32 * 1024 * 1024 + 5;
const INPUT_RESERVE: usize = 64 * 1024;
const MESSAGE_RESERVE: usize = 128;
const MOTION_BYTES: usize = 256; // conservative charge for one serialized motion event

#[derive(Clone, Default)]
pub struct Writer {
    inner: Arc<WriterInner>,
}

#[derive(Default)]
struct WriterInner {
    current: Mutex<Option<Arc<Connection>>>,
}

impl Drop for WriterInner {
    fn drop(&mut self) {
        if let Some(connection) = self.current.get_mut().unwrap().take() {
            connection.close();
        }
    }
}

struct Connection {
    // This handle only cancels I/O. It never writes or waits for the worker.
    cancel: TcpStream,
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Connection {
    fn close(&self) {
        let mut q = self.queue.lock().unwrap();
        q.closed = true;
        q.messages.clear();
        q.bytes = 0;
        drop(q);
        let _ = self.cancel.shutdown(Shutdown::Both);
        self.ready.notify_all();
    }
}

impl Writer {
    /// Whether the current connection still accepts outgoing messages.
    pub fn is_connected(&self) -> bool {
        self.inner
            .current
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|connection| !connection.queue.lock().unwrap().closed)
    }

    /// Install a fresh connection. Old queued events are discarded, never replayed.
    pub fn connect(&self, stream: TcpStream) -> io::Result<()> {
        let connection = Arc::new(Connection {
            cancel: stream.try_clone()?,
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
        });
        let worker = connection.clone();
        std::thread::Builder::new()
            .name("viewer-writer".into())
            .spawn(move || write_loop(worker, stream))?;
        let old = self.inner.current.lock().unwrap().replace(connection);
        if let Some(old) = old {
            old.close();
        }
        Ok(())
    }

    /// Wake blocked reads/writes immediately, including when Settings changes the address.
    pub fn disconnect(&self) {
        let old = self.inner.current.lock().unwrap().take();
        if let Some(old) = old {
            old.close();
        }
    }

    pub fn send(&self, tag: u8, json: &str) {
        self.enqueue(tag, json, false);
    }

    /// Clipboard payloads yield to input until the worker begins writing them. Offers and
    /// requests use `send`, retaining their place before any later paste shortcut.
    pub fn send_clipboard_data(&self, json: &str) {
        self.enqueue(1, json, true);
    }

    fn enqueue(&self, tag: u8, json: &str, bulk: bool) {
        let Some(connection) = self.inner.current.lock().unwrap().clone() else {
            return;
        };
        // Reject oversized messages before copying them. The server caps clipboard JSON at
        // 32 MiB and other messages at 1 MiB. Queue limits include the framing bytes.
        let cap = if tag == 1 { MAX_BYTES - 5 } else { 1 << 20 };
        if json.len() > cap {
            tracing::warn!("outgoing viewer message exceeds protocol limit; closing connection");
            connection.close();
            return;
        }
        let message = Message::new(tag, json, bulk);
        let mut q = connection.queue.lock().unwrap();
        if q.closed {
            return;
        }
        if !q.push(message) {
            // No blocking producer and no silent loss of an ordered key/button event. End
            // this session on overload; the reader reconnects with a new, empty queue.
            drop(q);
            tracing::warn!("outgoing viewer queue full; closing connection");
            connection.close();
            return;
        }
        drop(q);
        connection.ready.notify_one();
    }
}

#[derive(Default)]
struct Queue {
    messages: VecDeque<Message>,
    bytes: usize,
    closed: bool,
}

impl Queue {
    fn push(&mut self, message: Message) -> bool {
        if let (Some(Message::Motion(previous)), Message::Motion(next)) =
            (self.messages.back_mut(), &message)
        {
            if previous.merge(next) {
                return true;
            }
        }
        let (max_messages, max_bytes) = if message.bulk() {
            (MAX_MESSAGES - MESSAGE_RESERVE, MAX_BYTES)
        } else {
            (MAX_MESSAGES, MAX_BYTES + INPUT_RESERVE)
        };
        let bytes = message.bytes();
        if self.messages.len() >= max_messages || self.bytes + bytes > max_bytes {
            return false;
        }
        self.bytes += bytes;
        self.messages.push_back(message);
        true
    }

    fn pop(&mut self) -> Option<Message> {
        // Only overtake a prefix of bulk data with input. This preserves both clipboard
        // ordering (including new offers) and all non-bulk ordering, e.g. terminal resize
        // before terminal input. Motion preceding a button is the first input selected.
        let mut index = 0;
        for (i, message) in self.messages.iter().enumerate() {
            if message.bulk() {
                continue;
            }
            if message.input() {
                index = i;
            }
            break;
        }
        let message = self.messages.remove(index)?;
        self.bytes -= message.bytes();
        Some(message)
    }
}

enum Message {
    Motion(Motion),
    Frame { tag: u8, bytes: Vec<u8>, bulk: bool },
}

impl Message {
    fn new(tag: u8, json: &str, bulk: bool) -> Self {
        if tag == 0 {
            match serde_json::from_str::<InputMsg>(json) {
                Ok(InputMsg::PointerMove { monitor_id, x, y }) => {
                    return Self::Motion(Motion::Absolute { monitor_id, x, y });
                }
                Ok(InputMsg::PointerRelative { dx, dy }) => {
                    return Self::Motion(Motion::Relative { dx, dy });
                }
                _ => {}
            }
        }
        Self::Frame {
            tag,
            bytes: frame(tag, json.as_bytes()),
            bulk,
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Motion(_) => MOTION_BYTES,
            Self::Frame { bytes, .. } => bytes.len(),
        }
    }

    fn bulk(&self) -> bool {
        matches!(self, Self::Frame { bulk: true, .. })
    }

    fn input(&self) -> bool {
        matches!(self, Self::Motion(_) | Self::Frame { tag: 0 | 3, .. })
    }

    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Frame { bytes, .. } => bytes,
            Self::Motion(motion) => {
                let input = match motion {
                    Motion::Absolute { monitor_id, x, y } => {
                        InputMsg::PointerMove { monitor_id, x, y }
                    }
                    Motion::Relative { dx, dy } => InputMsg::PointerRelative { dx, dy },
                };
                frame(0, &serde_json::to_vec(&input).unwrap())
            }
        }
    }
}

enum Motion {
    Absolute { monitor_id: u32, x: f64, y: f64 },
    Relative { dx: f64, dy: f64 },
}

impl Motion {
    fn merge(&mut self, next: &Self) -> bool {
        match (self, next) {
            (
                Self::Absolute { monitor_id, x, y },
                Self::Absolute {
                    monitor_id: next_id,
                    x: nx,
                    y: ny,
                },
            ) if monitor_id == next_id => {
                *x = *nx;
                *y = *ny;
                true
            }
            (Self::Relative { dx, dy }, Self::Relative { dx: nx, dy: ny })
                if (*dx + nx).is_finite() && (*dy + ny).is_finite() =>
            {
                *dx += nx;
                *dy += ny;
                true
            }
            _ => false,
        }
    }
}

fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.push(tag);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

fn write_loop(connection: Arc<Connection>, mut stream: impl Write) {
    loop {
        let message = {
            let mut q = connection.queue.lock().unwrap();
            while !q.closed && q.messages.is_empty() {
                q = connection.ready.wait(q).unwrap();
            }
            if q.closed {
                break;
            }
            q.pop().unwrap()
        };
        // No queue or Writer mutex is held during serialization or socket I/O.
        if let Err(e) = stream.write_all(&message.into_bytes()) {
            tracing::warn!("viewer write failed: {e}");
            break;
        }
    }
    connection.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(3);
    const DOWN: &str = r#"{"kind":"key_code","keycode":47,"pressed":true}"#;
    const UP: &str = r#"{"kind":"key_code","keycode":47,"pressed":false}"#;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(WAIT)).unwrap();
        (stream, peer)
    }

    fn connection(stream: TcpStream) -> Arc<Connection> {
        Arc::new(Connection {
            cancel: stream,
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
        })
    }

    fn input(message: Message) -> InputMsg {
        let bytes = message.into_bytes();
        assert_eq!(bytes[0], 0);
        serde_json::from_slice(&bytes[5..]).unwrap()
    }

    #[test]
    fn coalescing_preserves_click_key_scroll_and_monitor_boundaries() {
        let mut q = Queue::default();
        for x in 0..1000 {
            assert!(q.push(Message::new(
                0,
                &format!(r#"{{"kind":"pointer_move","monitor_id":0,"x":{x},"y":2}}"#),
                false
            )));
        }
        assert_eq!(q.messages.len(), 1);
        for json in [
            r#"{"kind":"button","button":272,"pressed":true}"#,
            r#"{"kind":"pointer_move","monitor_id":0,"x":10,"y":3}"#,
            r#"{"kind":"pointer_move","monitor_id":1,"x":20,"y":4}"#,
            DOWN,
            UP,
            r#"{"kind":"pointer_relative","dx":1.5,"dy":-2}"#,
            r#"{"kind":"pointer_relative","dx":2,"dy":4}"#,
            r#"{"kind":"axis","axis":0,"step":1}"#,
            r#"{"kind":"pointer_relative","dx":5,"dy":6}"#,
        ] {
            assert!(q.push(Message::new(0, json, false)));
        }
        let mut got = Vec::new();
        while let Some(message) = q.pop() {
            got.push(input(message));
        }
        assert_eq!(
            got,
            vec![
                InputMsg::PointerMove {
                    monitor_id: 0,
                    x: 999.0,
                    y: 2.0
                },
                InputMsg::Button {
                    button: 272,
                    pressed: true
                },
                InputMsg::PointerMove {
                    monitor_id: 0,
                    x: 10.0,
                    y: 3.0
                },
                InputMsg::PointerMove {
                    monitor_id: 1,
                    x: 20.0,
                    y: 4.0
                },
                InputMsg::KeyCode {
                    keycode: 47,
                    pressed: true
                },
                InputMsg::KeyCode {
                    keycode: 47,
                    pressed: false
                },
                InputMsg::PointerRelative { dx: 3.5, dy: 2.0 },
                InputMsg::Axis { axis: 0, step: 1 },
                InputMsg::PointerRelative { dx: 5.0, dy: 6.0 },
            ]
        );
        assert_eq!(q.bytes, 0);
    }

    #[test]
    fn input_passes_bulk_but_clipboard_offers_and_terminal_resize_keep_order() {
        let mut q = Queue::default();
        for (tag, body, bulk) in [
            (1, "data1", true),
            (0, DOWN, false),
            (0, UP, false),
            (1, "offer2", false),
            (1, "data2", true),
            (4, "resize", false),
            (3, "terminal input", false),
        ] {
            assert!(q.push(Message::new(tag, body, bulk)));
        }
        for (tag, body) in [
            (0, DOWN),
            (0, UP),
            (1, "data1"),
            (1, "offer2"),
            (1, "data2"),
            (4, "resize"),
            (3, "terminal input"),
        ] {
            assert_eq!(q.pop().unwrap().into_bytes(), frame(tag, body.as_bytes()));
        }
        assert!(q.messages.is_empty());
    }

    #[test]
    fn queue_bounds_bytes_and_count_and_reserves_room_for_releases() {
        let mut q = Queue::default();
        // The largest legal clipboard frame fits, while another payload does not.
        assert!(q.push(Message::Frame {
            tag: 1,
            bytes: vec![0; MAX_BYTES],
            bulk: true
        }));
        assert!(!q.push(Message::new(1, "extra", true)));
        assert!(q.push(Message::new(0, UP, false)));
        assert_eq!(q.pop().unwrap().into_bytes(), frame(0, UP.as_bytes()));
        q.pop().unwrap();
        assert_eq!(q.bytes, 0);

        for _ in 0..MAX_MESSAGES - MESSAGE_RESERVE {
            assert!(q.push(Message::new(1, "data", true)));
        }
        assert!(!q.push(Message::new(1, "extra", true)));
        for _ in 0..MESSAGE_RESERVE {
            assert!(q.push(Message::new(0, UP, false)));
        }
        assert!(!q.push(Message::new(0, DOWN, false)));
    }

    // Deterministic stalled transport: the first actual write parks until explicitly
    // released. No dependency on a particular OS send-buffer size or network speed.
    struct StalledSink {
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
        output: mpsc::Sender<Vec<u8>>,
    }

    impl Write for StalledSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv_timeout(WAIT).unwrap();
            }
            self.output.send(bytes.to_vec()).unwrap();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stalled_write_does_not_block_producers_and_preserves_wire_order() {
        let (stream, _peer) = pair();
        let connection = connection(stream);
        let writer = Writer::default();
        *writer.inner.current.lock().unwrap() = Some(connection.clone());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (output_tx, output_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            write_loop(
                connection,
                StalledSink {
                    entered: Some(entered_tx),
                    release: release_rx,
                    output: output_tx,
                },
            )
        });
        writer.send_clipboard_data("in flight");
        entered_rx.recv_timeout(WAIT).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let producer = writer.clone();
        let producer = std::thread::spawn(move || {
            producer.send_clipboard_data("queued");
            for x in 0..1000 {
                producer.send(
                    0,
                    &format!(r#"{{"kind":"pointer_move","monitor_id":0,"x":{x},"y":0}}"#),
                );
            }
            producer.send(0, DOWN);
            producer.send(0, UP);
            done_tx.send(()).unwrap();
        });
        let completed_while_stalled = done_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).unwrap();
        assert!(
            completed_while_stalled.is_ok(),
            "producer waited for socket I/O"
        );
        producer.join().unwrap();
        assert_eq!(
            output_rx.recv_timeout(WAIT).unwrap(),
            frame(1, b"in flight")
        );
        let motion = output_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(
            serde_json::from_slice::<InputMsg>(&motion[5..]).unwrap(),
            InputMsg::PointerMove {
                monitor_id: 0,
                x: 999.0,
                y: 0.0
            }
        );
        for (tag, body) in [(0, DOWN), (0, UP), (1, "queued")] {
            assert_eq!(
                output_rx.recv_timeout(WAIT).unwrap(),
                frame(tag, body.as_bytes())
            );
        }
        writer.disconnect();
        worker.join().unwrap();
    }

    #[test]
    fn disconnect_wakes_blocked_tcp_writer_and_reader() {
        let (stream, mut peer) = pair();
        let mut reader = stream.try_clone().unwrap();
        reader.set_read_timeout(Some(WAIT)).unwrap();
        let connection = connection(stream.try_clone().unwrap());
        let writer = Writer::default();
        *writer.inner.current.lock().unwrap() = Some(connection.clone());
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            write_loop(connection, stream);
            done_tx.send(()).unwrap();
        });
        // Far larger than the loopback socket's send buffer, with a peer that never drains.
        writer.send_clipboard_data(&"x".repeat(16 * 1024 * 1024));
        peer.read_exact(&mut [0]).unwrap(); // worker has entered the real write
        writer.send(0, DOWN);
        writer.disconnect();
        done_rx
            .recv_timeout(WAIT)
            .expect("blocked socket writer was not cancelled");
        assert!(matches!(reader.read(&mut [0]), Ok(0) | Err(_)));
        worker.join().unwrap();
    }

    #[test]
    fn old_connection_failure_and_pending_input_do_not_leak_into_reconnect() {
        let (old_stream, _old_peer) = pair();
        let old = connection(old_stream);
        let writer = Writer::default();
        assert!(!writer.is_connected());
        *writer.inner.current.lock().unwrap() = Some(old.clone());
        writer.send(0, DOWN); // no old worker: deterministically still queued
        let (new_stream, mut new_peer) = pair();
        writer.connect(new_stream).unwrap();
        assert!(writer.is_connected());
        assert!(old.queue.lock().unwrap().messages.is_empty());
        old.close(); // late completion/error from the old worker
        writer.send(0, UP);
        let expected = frame(0, UP.as_bytes());
        let mut got = vec![0; expected.len()];
        new_peer.read_exact(&mut got).unwrap();
        assert_eq!(got, expected);
        writer.disconnect();
        assert!(!writer.is_connected());
        assert_eq!(new_peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn concurrent_pointer_and_ui_threads_preserve_deltas_keys_and_framing() {
        let (stream, mut peer) = pair();
        let writer = Writer::default();
        writer.connect(stream).unwrap();
        let receiver = std::thread::spawn(move || {
            let mut keys = Vec::new();
            let (mut x, mut y) = (0.0, 0.0);
            while keys.len() < 200 || x < 600.0 {
                let mut header = [0; 5];
                peer.read_exact(&mut header).unwrap();
                assert_eq!(header[0], 0);
                let len = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
                assert!(len < 256, "interleaved frame headers");
                let mut body = vec![0; len];
                peer.read_exact(&mut body).unwrap();
                match serde_json::from_slice::<InputMsg>(&body).unwrap() {
                    InputMsg::PointerRelative { dx, dy } => {
                        x += dx;
                        y += dy;
                    }
                    InputMsg::KeyCode {
                        keycode: 47,
                        pressed,
                    } => keys.push(pressed),
                    other => panic!("unexpected input: {other:?}"),
                }
            }
            assert_eq!((x, y), (600.0, -800.0));
            assert_eq!(keys, [true, false].repeat(100));
        });
        let pointer = writer.clone();
        let pointer = std::thread::spawn(move || {
            for _ in 0..400 {
                pointer.send(0, r#"{"kind":"pointer_relative","dx":1.5,"dy":-2}"#);
            }
        });
        let keyboard = writer.clone();
        let keyboard = std::thread::spawn(move || {
            for _ in 0..100 {
                keyboard.send(0, DOWN);
                keyboard.send(0, UP);
            }
        });
        pointer.join().unwrap();
        keyboard.join().unwrap();
        receiver.join().unwrap();
        writer.disconnect();
    }

    #[test]
    fn overload_closes_session_instead_of_silently_dropping_ordered_input() {
        let (stream, mut peer) = pair();
        let connection = connection(stream);
        let writer = Writer::default();
        *writer.inner.current.lock().unwrap() = Some(connection.clone());
        for _ in 0..MAX_MESSAGES + 1 {
            writer.send(0, DOWN);
        }
        let q = connection.queue.lock().unwrap();
        assert!(q.closed);
        assert!(q.messages.is_empty());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
}
