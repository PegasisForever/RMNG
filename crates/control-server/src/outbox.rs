//! One browser viewer's send queue, and what the browser's acknowledgements say about its path.
//!
//! - **Lanes.** Control messages (every text message) go first, so a cursor move or an `echo`
//!   never waits behind queued frames. Then video, then clipboard replies, cut into pieces
//!   ([`wire::browser::CLIPBOARD_PIECE_BYTES`]) as they go: a piece goes only when no video
//!   waits, the viewer has room as for a frame ([`State::may_send`]), and the bytes on the way
//!   stay within what the path delivers in [`CLIP_WINDOW`] (at least one piece), so a large
//!   copy holds video back by about that long. The writer ([`crate::webviewer`]) takes from
//!   [`Outbox::next`], which stamps each video frame as it hands it to the socket.
//! - **Acknowledgements.** The browser sends the newest frame it decoded, how long it held
//!   that number, and how many clipboard pieces arrived ([`wire::browser::FromBrowser::Ack`]).
//!   Each one gives a round trip (from the stamp, less the hold) and a floor (the lowest round
//!   trip of the last 1–2 minutes, of deltas sent while no keyframe was out), and ends the
//!   frames it covers. The bytes they end give the delivered rate ([`Meter`]).
//! - **Behind** ([`State::behind`]): how long the oldest video frame that is not acknowledged
//!   (queued here or on the way) has waited since it was queued, less the floor. One number
//!   for our queue, the kernel's, any proxy's, the path's and the browser's decoder.
//! - **Room** ([`Outbox::room`]): whether the encoder may make another frame for this viewer
//!   (R1 in `docs/WEB-VIEWER-WAN-SCOPE.md`; the media plane holds the newest captured frame
//!   until every viewer of its monitor has room). Why the rule has its parts is in
//!   `docs/WAN-BENCH.md` (round 3).
//! - **Stall** ([`Outbox::stalled`]): frames or a clipboard piece on the way and no
//!   acknowledgement that moves for [`STALL`]. The socket is closed only then, whatever one
//!   write takes.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use wire::browser::{CLIPBOARD_PIECE_BYTES, encode_clipboard_piece};
use wire::socket::ClipboardData;

use crate::mediaplane::Queued;

/// A frame that waited [`MAX_HOLD`] gets room while the viewer is at most this far behind.
const BEHIND_MAX: Duration = Duration::from_secs(1);
/// Frames on the way and no acknowledgement that moves for this long: the viewer is stuck.
const STALL: Duration = Duration::from_secs(10);
/// More bytes than this queued in all lanes closes the viewer, so one that stops reading
/// cannot hold more memory: two of the largest clipboard replies.
const MAX_QUEUED_BYTES: usize = 2 * wire::socket::chunk::MAX_MESSAGE_BYTES;
/// The floor is the lowest round trip of this bucket and the one before, so of the last 1 to
/// 2 minutes. Long enough that a queue which stands for a while (a scroll on a slow link) does
/// not lift it, short enough to follow a new route (Wi-Fi to a phone) within two minutes.
const FLOOR_BUCKET: Duration = Duration::from_secs(60);
/// An acknowledgement held longer than its frame has been out names an earlier frame with the
/// same number (a new encoder counts from 1 again). This much is rounding.
const HELD_SLACK: Duration = Duration::from_millis(5);
/// A viewer has room while it is at most this plus one floor behind: its oldest frame not
/// acknowledged has been out at most two round trips and this. The second round trip is for
/// TCP's repair of a lost packet; acknowledgements come up to 50 ms late (`ACK_MS` in
/// `viewer/worker.ts`).
const ROOM: Duration = Duration::from_millis(100);
/// A frame that has waited this long for room gets it while the viewer is at most
/// [`BEHIND_MAX`] behind: a frame held longer covers more motion and costs more than its share
/// of bytes. No timer: the media plane looks at a held frame again only at the next captured
/// frame, acknowledgement or leave.
const MAX_HOLD: Duration = Duration::from_millis(100);
/// The debug summary comes at most this often.
const LOG_EVERY: Duration = Duration::from_secs(5);
/// Clipboard pieces on the way (with the video on the way) stay within what the path
/// delivered in this long, so the video behind them waits about this long at most. At least one
/// piece is always allowed; on the LAN the window is megabytes.
const CLIP_WINDOW: Duration = Duration::from_millis(100);
/// The delivered rate is measured over periods of this length.
const METER_PERIOD: Duration = Duration::from_millis(500);

pub(crate) struct Outbox {
    state: Mutex<State>,
    /// The writer waits here for a message.
    ready: Condvar,
    /// Wakes the writer out of a send when the viewer is closed.
    closing: tokio::sync::Notify,
}

/// What [`Outbox::next`] gives the writer.
pub(crate) enum Next {
    Msg(Queued),
    /// Nothing came in the time given.
    Idle,
    Closed,
}

struct State {
    control: VecDeque<Queued>,
    /// Video not yet handed to the socket, with the time it was queued.
    video: VecDeque<(Queued, Instant)>,
    /// Clipboard replies not yet sent, and how many of each one's bytes went already.
    clip: VecDeque<(Arc<ClipboardData>, usize)>,
    /// Video handed to the socket and not yet acknowledged, oldest first.
    in_flight: VecDeque<Sent>,
    /// Clipboard pieces handed to the socket, and those the browser says arrived (both wrap).
    clip_sent: u32,
    clip_acked: u32,
    /// The sizes of the pieces on the way, oldest first.
    clip_out: VecDeque<usize>,
    /// Since when the next clipboard piece waits for room.
    clip_wait: Option<Instant>,
    queued_bytes: usize,
    closed: bool,
    /// The keyframe with this number is queued or on the way: the deltas sent until it is
    /// acknowledged do not count for the floor.
    key_out: Option<u32>,
    /// When an acknowledgement last moved, or something went out with nothing on the way.
    progress_at: Instant,
    floor: Floor,
    meter: Meter,
    /// Video frames acknowledged since [`Outbox::take_shown`] last took them.
    shown: u32,
    period: Period,
}

/// The rate at which acknowledgements end bytes (video and clipboard pieces), over the last
/// complete [`METER_PERIOD`].
struct Meter {
    start: Instant,
    bytes: usize,
    /// Bytes per second; 0 until one period is complete.
    rate: f64,
}

impl Meter {
    fn add(&mut self, bytes: usize, now: Instant) {
        self.bytes += bytes;
        let secs = now.saturating_duration_since(self.start).as_secs_f64();
        if secs >= METER_PERIOD.as_secs_f64() {
            self.rate = self.bytes as f64 / secs;
            (self.start, self.bytes) = (now, 0);
        }
    }
}

#[derive(Clone, Copy)]
struct Sent {
    seq: u32,
    len: usize,
    queued: Instant,
    sent: Instant,
    /// A delta handed to the socket while no keyframe was out. Only its round trip counts for
    /// the floor: a keyframe's, and those of the deltas behind it, are longer than the path's,
    /// and every join starts with a keyframe.
    for_floor: bool,
}

/// The lowest round trip of two buckets of [`FLOOR_BUCKET`].
struct Floor {
    start: Instant,
    cur: Option<Duration>,
    prev: Option<Duration>,
}

impl Floor {
    fn add(&mut self, rtt: Duration, now: Instant) {
        let age = now.saturating_duration_since(self.start);
        if age >= 2 * FLOOR_BUCKET {
            (self.prev, self.cur, self.start) = (None, None, now);
        } else if age >= FLOOR_BUCKET {
            (self.prev, self.cur) = (self.cur, None);
            self.start += FLOOR_BUCKET;
        }
        self.cur = Some(self.cur.map_or(rtt, |c| c.min(rtt)));
    }

    fn get(&self) -> Option<Duration> {
        match (self.cur, self.prev) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// Counts for the summary line, since the last one.
struct Period {
    start: Instant,
    rtt_sum: Duration,
    rtts: u32,
    acked_bytes: usize,
    clip_bytes: usize,
    /// Captured frames of this viewer's monitor that were never encoded (R1).
    not_encoded: u32,
    /// Each acknowledged frame's time from our queue to its acknowledgement, less the hold.
    delays: Vec<Duration>,
}

impl Period {
    fn new(start: Instant) -> Period {
        Period {
            start,
            rtt_sum: Duration::ZERO,
            rtts: 0,
            acked_bytes: 0,
            clip_bytes: 0,
            not_encoded: 0,
            delays: Vec::new(),
        }
    }
}

fn len(msg: &Queued) -> usize {
    match msg {
        Queued::Video { bytes, .. } | Queued::Binary(bytes) => bytes.len(),
        Queued::Clipboard(d) => d.bytes.len(),
        Queued::Text(t) => t.len(),
    }
}

impl State {
    /// How far behind the viewer is at `now` (see the module docs).
    fn behind(&self, now: Instant) -> Duration {
        let oldest = match self.in_flight.front() {
            Some(f) => f.queued,
            None => match self.video.front() {
                Some((_, t)) => *t,
                None => return Duration::ZERO,
            },
        };
        now.saturating_duration_since(oldest)
            .saturating_sub(self.floor.get().unwrap_or_default())
    }

    /// At most [`ROOM`] plus one floor behind.
    fn has_room(&self, now: Instant) -> bool {
        self.behind(now) <= ROOM + self.floor.get().unwrap_or_default()
    }

    /// Whether something that has waited `held` may go: with room, or after [`MAX_HOLD`] while
    /// at most [`BEHIND_MAX`] behind.
    fn may_send(&self, now: Instant, held: Duration) -> bool {
        self.has_room(now) || (held >= MAX_HOLD && self.behind(now) <= BEHIND_MAX)
    }

    /// Whether a clipboard piece of `len` bytes fits in [`CLIP_WINDOW`] with what is on the way.
    fn clip_fits(&self, len: usize) -> bool {
        let out: usize = self.in_flight.iter().map(|f| f.len).sum::<usize>()
            + self.clip_out.iter().sum::<usize>();
        self.clip_out.is_empty()
            || (out + len) as f64 <= self.meter.rate * CLIP_WINDOW.as_secs_f64()
    }

    /// Something is on the way that an acknowledgement must end.
    fn waiting(&self) -> bool {
        !self.in_flight.is_empty() || self.clip_sent != self.clip_acked
    }

    /// The size of the next piece of the first clipboard reply.
    fn next_piece_len(&self) -> Option<usize> {
        let (d, off) = self.clip.front()?;
        Some((d.bytes.len() - off).min(CLIPBOARD_PIECE_BYTES))
    }

    /// The next piece of the first clipboard reply, as a binary message.
    fn next_piece(&mut self) -> Option<Vec<u8>> {
        let (d, off) = self.clip.front_mut()?;
        let end = (*off + CLIPBOARD_PIECE_BYTES).min(d.bytes.len());
        let last = end == d.bytes.len();
        let piece = encode_clipboard_piece(d, &d.bytes[*off..end], last);
        self.queued_bytes -= end - *off;
        self.period.clip_bytes += end - *off;
        self.clip_out.push_back(end - *off);
        if last {
            self.clip.pop_front();
        } else {
            *off = end;
        }
        Some(piece)
    }
}

impl Outbox {
    pub(crate) fn new() -> Outbox {
        let now = Instant::now();
        Outbox {
            state: Mutex::new(State {
                control: VecDeque::new(),
                video: VecDeque::new(),
                clip: VecDeque::new(),
                in_flight: VecDeque::new(),
                clip_sent: 0,
                clip_acked: 0,
                clip_out: VecDeque::new(),
                clip_wait: None,
                queued_bytes: 0,
                closed: false,
                key_out: None,
                progress_at: now,
                floor: Floor {
                    start: now,
                    cur: None,
                    prev: None,
                },
                meter: Meter {
                    start: now,
                    bytes: 0,
                    rate: 0.0,
                },
                shown: 0,
                period: Period::new(now),
            }),
            ready: Condvar::new(),
            closing: tokio::sync::Notify::new(),
        }
    }

    /// Queue one message. False when the viewer is closed or holds too much: the caller then
    /// removes and closes it.
    pub(crate) fn push(&self, msg: Queued) -> bool {
        self.push_at(msg, Instant::now())
    }

    fn push_at(&self, msg: Queued, now: Instant) -> bool {
        // Pieces are cut in `next`; one from outside would skip the clipboard lane's rules.
        assert!(!matches!(msg, Queued::Binary(_)), "only the outbox cuts clipboard pieces");
        let mut st = self.state.lock().unwrap();
        if st.closed {
            return false;
        }
        st.queued_bytes += len(&msg);
        match msg {
            Queued::Video { seq, key, .. } => {
                if key {
                    st.key_out = Some(seq);
                }
                st.video.push_back((msg, now));
            }
            Queued::Clipboard(d) => st.clip.push_back((d, 0)),
            _ => st.control.push_back(msg),
        }
        if st.queued_bytes > MAX_QUEUED_BYTES {
            return false;
        }
        self.ready.notify_one();
        true
    }

    /// The next message to send: control, then video, then a clipboard piece. Waits up to
    /// `timeout`, and wakes on its own when a waiting piece has waited [`MAX_HOLD`]. A video
    /// frame or a clipboard piece counts as handed to the socket from here.
    pub(crate) fn next(&self, timeout: Duration) -> Next {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.lock().unwrap();
        loop {
            if st.closed {
                return Next::Closed;
            }
            if let Some(msg) = st.control.pop_front() {
                st.queued_bytes -= len(&msg);
                return Next::Msg(msg);
            }
            let now = Instant::now();
            if let Some((msg, queued)) = st.video.pop_front() {
                let n = len(&msg);
                st.queued_bytes -= n;
                let Queued::Video { seq, key, .. } = msg else {
                    unreachable!("the video lane holds video")
                };
                if !st.waiting() {
                    st.progress_at = now;
                }
                let for_floor = !key && st.key_out.is_none();
                st.in_flight.push_back(Sent {
                    seq,
                    len: n,
                    queued,
                    sent: now,
                    for_floor,
                });
                return Next::Msg(msg);
            }
            let mut wait = deadline.saturating_duration_since(now);
            if let Some(len) = st.next_piece_len() {
                let since = *st.clip_wait.get_or_insert(now);
                let held = now.saturating_duration_since(since);
                if st.may_send(now, held) && st.clip_fits(len) {
                    if !st.waiting() {
                        st.progress_at = now;
                    }
                    let piece = st.next_piece().expect("a piece is queued");
                    st.clip_sent = st.clip_sent.wrapping_add(1);
                    st.clip_wait = None;
                    return Next::Msg(Queued::Binary(piece.into()));
                }
                if held < MAX_HOLD {
                    wait = wait.min(MAX_HOLD - held);
                }
            }
            if wait.is_zero() {
                return Next::Idle;
            }
            st = self.ready.wait_timeout(st, wait).unwrap().0;
        }
    }

    /// A newer copy: drop the clipboard replies not sent yet. Pieces already sent stay sent; the
    /// browser drops a reply it got only part of when it sees the new copy's offer.
    pub(crate) fn cancel_clipboard(&self) {
        let mut st = self.state.lock().unwrap();
        let left: usize = st.clip.drain(..).map(|(d, off)| d.bytes.len() - off).sum();
        st.queued_bytes -= left;
        st.clip_wait = None;
    }

    /// Drop everything and stop the writer, also inside a send.
    pub(crate) fn close(&self) {
        let mut st = self.state.lock().unwrap();
        st.closed = true;
        st.control.clear();
        st.video.clear();
        st.clip.clear();
        st.queued_bytes = 0;
        self.ready.notify_all();
        // Stores a permit when the writer is not waiting yet.
        self.closing.notify_one();
    }

    /// Completes when [`Outbox::close`] was called. The writer sends inside a `select!` on it.
    pub(crate) async fn closed(&self) {
        self.closing.notified().await
    }

    /// The browser decoded every frame up to `seq` and held that number for `held`, and `clip`
    /// clipboard pieces arrived.
    pub(crate) fn ack(&self, seq: u32, held: Duration, clip: u32, now: Instant) {
        let mut st = self.state.lock().unwrap();
        // The first match. After an encoder restart (a new size) the new encoder counts from 1
        // again, so an older frame on the way can have the same number: then this ends less
        // than it could and the round trip is too long, until the old frames are acknowledged.
        if let Some(i) = st.in_flight.iter().position(|f| f.seq == seq) {
            let for_floor = st.in_flight[i].for_floor;
            let out = now.saturating_duration_since(st.in_flight[i].sent);
            if held <= out + HELD_SLACK {
                let rtt = out.saturating_sub(held);
                let delay = now.saturating_duration_since(st.in_flight[i].queued);
                st.period.delays.push(delay.saturating_sub(held));
                for f in st.in_flight.drain(..=i).collect::<Vec<_>>() {
                    st.period.acked_bytes += f.len;
                    st.meter.add(f.len, now);
                    st.shown += 1;
                    if st.key_out == Some(f.seq) {
                        st.key_out = None;
                    }
                }
                st.progress_at = now;
                if for_floor {
                    st.floor.add(rtt, now);
                }
                st.period.rtt_sum += rtt;
                st.period.rtts += 1;
            }
        }
        // Only a count between the last acknowledged and the last sent.
        let new = clip.wrapping_sub(st.clip_acked);
        if new != 0 && new <= st.clip_sent.wrapping_sub(st.clip_acked) {
            st.clip_acked = clip;
            st.progress_at = now;
            let bytes: usize = st.clip_out.drain(..new as usize).sum();
            st.meter.add(bytes, now);
        }
        // Room for a clipboard piece may have come.
        self.ready.notify_one();
    }

    /// Whether the encoder may make another frame for this viewer, when a frame has waited
    /// `held` for room: at most one frame waits in our queue (the socket writes it while the
    /// next one is encoded), and the viewer is at most [`ROOM`] plus the floor behind; or, once
    /// the frame has waited [`MAX_HOLD`], at most [`BEHIND_MAX`] behind. A keyframe on the way
    /// counts as any frame: so the frames behind a slow keyframe come at least 10 per second
    /// and do not fill the path. A viewer that stops acknowledging holds the others back until
    /// [`STALL`] closes it.
    pub(crate) fn room(&self, now: Instant, held: Duration) -> bool {
        let st = self.state.lock().unwrap();
        st.video.len() <= 1 && st.may_send(now, held)
    }

    /// The bytes per second the acknowledgements ended lately (video and clipboard), or `None`
    /// before the first measurement. For the automatic bitrate ([`crate::bitrate`]).
    pub(crate) fn delivered(&self) -> Option<f64> {
        let rate = self.state.lock().unwrap().meter.rate;
        (rate > 0.0).then_some(rate)
    }

    /// The video frames acknowledged since the last call, and whether a keyframe is queued or on
    /// the way.
    pub(crate) fn take_shown(&self) -> (u32, bool) {
        let mut st = self.state.lock().unwrap();
        (std::mem::take(&mut st.shown), st.key_out.is_some())
    }

    /// A captured frame of this viewer's monitor was replaced before it was encoded.
    pub(crate) fn not_encoded(&self) {
        self.state.lock().unwrap().period.not_encoded += 1;
    }

    /// Frames or a clipboard piece are on the way and no acknowledgement has moved for
    /// [`STALL`].
    pub(crate) fn stalled(&self, now: Instant) -> bool {
        let st = self.state.lock().unwrap();
        st.waiting() && now.saturating_duration_since(st.progress_at) >= STALL
    }

    /// The summary for the debug log, at most once per [`LOG_EVERY`], and only when video
    /// or clipboard bytes moved or wait.
    pub(crate) fn summary(&self, now: Instant) -> Option<String> {
        let mut st = self.state.lock().unwrap();
        let secs = now.saturating_duration_since(st.period.start).as_secs_f64();
        if secs < LOG_EVERY.as_secs_f64() {
            return None;
        }
        let mut p = std::mem::replace(&mut st.period, Period::new(now));
        if p.acked_bytes == 0 && p.clip_bytes == 0 && !st.waiting() && st.video.is_empty() {
            return None;
        }
        let ms = |d: Duration| format!("{:.0}", d.as_secs_f64() * 1000.0);
        let rtt = match p.rtts {
            0 => "-".into(),
            n => ms(p.rtt_sum / n),
        };
        p.delays.sort_unstable();
        let pct = |q: f64| match p.delays.len() {
            0 => "-".into(),
            n => ms(p.delays[((n - 1) as f64 * q).round() as usize]),
        };
        let in_flight_bytes: usize = st.in_flight.iter().map(|f| f.len).sum();
        Some(format!(
            "rtt {rtt} ms, delay p50 {} ms p95 {} ms, floor {} ms, behind {} ms, in flight {} \
             frames {} KB, queued {} frames, {:.2} Mbit/s delivered, {} KB clipboard, {} not \
             encoded",
            pct(0.5),
            pct(0.95),
            st.floor.get().map_or("-".into(), ms),
            ms(st.behind(now)),
            st.in_flight.len(),
            in_flight_bytes / 1000,
            st.video.len(),
            p.acked_bytes as f64 * 8.0 / secs / 1e6,
            p.clip_bytes / 1000,
            p.not_encoded,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(seq: u32, key: bool) -> Queued {
        Queued::Video {
            monitor_id: 0,
            seq,
            key,
            bytes: Arc::from(vec![0u8; 1000]),
        }
    }

    fn text(s: &str) -> Queued {
        Queued::Text(s.into())
    }

    fn clipboard(serial: u64, n: usize) -> Queued {
        Queued::Clipboard(Arc::new(ClipboardData {
            serial,
            mime_type: "image/png".into(),
            bytes: vec![7; n],
        }))
    }

    /// The writer's next message, which must be there already.
    fn take(o: &Outbox) -> Queued {
        match o.next(Duration::ZERO) {
            Next::Msg(m) => m,
            _ => panic!("nothing queued"),
        }
    }

    /// The writer's next message within `timeout`.
    fn take_within(o: &Outbox, timeout: Duration) -> Queued {
        match o.next(timeout) {
            Next::Msg(m) => m,
            _ => panic!("nothing came"),
        }
    }

    fn seq(m: &Queued) -> u32 {
        match m {
            Queued::Video { seq, .. } => *seq,
            _ => panic!("not video: {m:?}"),
        }
    }

    /// A clipboard piece: its serial, its length and whether it is the last.
    fn piece(m: &Queued) -> (u64, usize, bool) {
        let Queued::Binary(b) = m else {
            panic!("not a clipboard piece: {m:?}")
        };
        let (d, last) = wire::browser::decode_clipboard_piece(b).unwrap();
        (d.serial, d.bytes.len(), last)
    }

    #[test]
    fn control_goes_before_queued_video() {
        let o = Outbox::new();
        assert!(o.push(video(1, true)));
        assert!(o.push(video(2, false)));
        assert!(o.push(text("cursor")));
        assert_eq!(take(&o), text("cursor"));
        assert_eq!(seq(&take(&o)), 1);
        assert_eq!(seq(&take(&o)), 2);
        assert!(matches!(o.next(Duration::ZERO), Next::Idle));
    }

    /// A clipboard reply goes after video, in pieces, each one only once the one before it
    /// arrived.
    #[test]
    fn a_clipboard_reply_goes_in_pieces_after_video_one_at_a_time() {
        let o = Outbox::new();
        let n = CLIPBOARD_PIECE_BYTES + 10;
        o.push(clipboard(5, n));
        o.push(video(1, false));
        assert_eq!(seq(&take(&o)), 1);
        let t = o.state.lock().unwrap().in_flight[0].sent;
        o.ack(1, Duration::ZERO, 0, t);
        assert_eq!(piece(&take(&o)), (5, CLIPBOARD_PIECE_BYTES, false));
        assert!(
            matches!(o.next(Duration::ZERO), Next::Idle),
            "a piece is on the way"
        );
        o.ack(1, Duration::ZERO, 1, t);
        assert_eq!(piece(&take(&o)), (5, 10, true));
        o.ack(1, Duration::ZERO, 2, t);
        assert!(matches!(o.next(Duration::ZERO), Next::Idle));
        assert_eq!(o.state.lock().unwrap().queued_bytes, 0);
    }

    /// No clipboard piece while the viewer has no room: it would hold back the video behind it.
    #[test]
    fn no_clipboard_piece_without_room() {
        let o = Outbox::new();
        // Frame 1 was queued longer than `ROOM` ago (no floor yet).
        let t0 = Instant::now().checked_sub(ROOM * 2).unwrap();
        o.push_at(video(1, false), t0);
        take(&o);
        o.push(clipboard(5, 10));
        assert!(matches!(o.next(Duration::ZERO), Next::Idle));
        o.ack(1, Duration::ZERO, 0, Instant::now());
        assert_eq!(piece(&take(&o)), (5, 10, true));
    }

    /// A piece that waited `MAX_HOLD` goes while the viewer is at most `BEHIND_MAX` behind, as a
    /// frame does: else a full link that gets frames by that rule would never send a piece.
    #[test]
    fn a_clipboard_piece_that_waited_goes_as_a_held_frame_does() {
        let o = Outbox::new();
        let t0 = Instant::now().checked_sub(ROOM * 3).unwrap();
        o.push_at(video(1, false), t0);
        take(&o);
        o.push(clipboard(5, 10));
        let t = Instant::now();
        assert!(matches!(o.next(Duration::ZERO), Next::Idle), "no room yet");
        // The writer wakes on its own once the piece has waited `MAX_HOLD`.
        assert_eq!(piece(&take_within(&o, MAX_HOLD * 2)), (5, 10, true));
        assert!(t.elapsed() >= MAX_HOLD);
    }

    /// Once the path has shown its rate, the pieces on the way may fill `CLIP_WINDOW` of it.
    #[test]
    fn clipboard_pieces_fill_a_window_of_the_delivered_rate() {
        let o = Outbox::new();
        o.push(clipboard(5, 10 * CLIPBOARD_PIECE_BYTES));
        take(&o);
        assert!(matches!(o.next(Duration::ZERO), Next::Idle), "no rate yet: one piece");
        // The piece arrived after `METER_PERIOD`: the rate is one piece per period, so the
        // window holds a fifth of a piece, and one piece goes at a time.
        let t = o.state.lock().unwrap().meter.start + METER_PERIOD;
        o.ack(0, Duration::ZERO, 1, t);
        take(&o);
        assert!(matches!(o.next(Duration::ZERO), Next::Idle));
        // Twenty pieces per period: the window holds four.
        o.state.lock().unwrap().meter.rate =
            20.0 * CLIPBOARD_PIECE_BYTES as f64 / METER_PERIOD.as_secs_f64();
        for _ in 0..3 {
            take(&o);
        }
        assert!(matches!(o.next(Duration::ZERO), Next::Idle));
        o.ack(0, Duration::ZERO, 3, Instant::now());
        take(&o);
        take(&o);
        assert!(matches!(o.next(Duration::ZERO), Next::Idle));
    }

    /// A newer copy drops what is left of the replies not sent yet.
    #[test]
    fn a_newer_copy_cancels_the_rest_of_a_reply() {
        let o = Outbox::new();
        o.push(clipboard(5, 3 * CLIPBOARD_PIECE_BYTES));
        take(&o);
        o.cancel_clipboard();
        o.ack(0, Duration::ZERO, 1, Instant::now());
        assert!(matches!(o.next(Duration::ZERO), Next::Idle));
        assert_eq!(o.state.lock().unwrap().queued_bytes, 0);
        o.push(clipboard(6, 1));
        assert_eq!(piece(&take(&o)), (6, 1, true));
    }

    /// An acknowledgement ends every frame up to it and gives one round trip, less the hold.
    #[test]
    fn an_ack_gives_a_round_trip_and_ends_the_frames_before_it() {
        let o = Outbox::new();
        for s in 1..=3 {
            o.push(video(s, false));
            take(&o);
        }
        let sent = o.state.lock().unwrap().in_flight[1].sent;
        let ms = Duration::from_millis;
        o.ack(2, ms(10), 0, sent + ms(60));
        let st = o.state.lock().unwrap();
        assert_eq!(st.in_flight.len(), 1);
        assert_eq!(st.floor.get(), Some(ms(50)));
        assert_eq!(st.period.acked_bytes, 2000);
    }

    /// A keyframe, and a delta sent while a keyframe is on the way, do not give the floor:
    /// their round trips are longer than the path's.
    #[test]
    fn the_floor_skips_keyframes_and_the_deltas_behind_them() {
        let o = Outbox::new();
        let ms = Duration::from_millis;
        o.push(video(1, true));
        o.push(video(2, false));
        take(&o);
        take(&o);
        let sent = o.state.lock().unwrap().in_flight[1].sent;
        o.ack(1, ms(0), 0, sent + ms(400));
        o.ack(2, ms(0), 0, sent + ms(450));
        assert_eq!(o.state.lock().unwrap().floor.get(), None);
        o.push(video(3, false));
        take(&o);
        let sent = o.state.lock().unwrap().in_flight[0].sent;
        o.ack(3, ms(0), 0, sent + ms(150));
        assert_eq!(o.state.lock().unwrap().floor.get(), Some(ms(150)));
    }

    /// A hold longer than the frame has been out is an earlier frame's number: no round trip,
    /// and nothing ends.
    #[test]
    fn an_ack_held_longer_than_the_frame_is_out_is_ignored() {
        let o = Outbox::new();
        o.push(video(1, true));
        take(&o);
        let sent = o.state.lock().unwrap().in_flight[0].sent;
        o.ack(
            1,
            Duration::from_secs(5),
            0,
            sent + Duration::from_millis(20),
        );
        let st = o.state.lock().unwrap();
        assert_eq!(st.in_flight.len(), 1);
        assert_eq!(st.floor.get(), None);
    }

    /// Room: at most one frame queued here, and the oldest frame on the way out at most two
    /// floors and `ROOM`; or, for a frame held `MAX_HOLD`, less than `BEHIND_MAX` behind, also
    /// while a keyframe is on the way. Far behind, nothing is dropped: the next frame is a delta.
    #[test]
    fn room_needs_a_short_queue_and_a_short_wait() {
        let o = Outbox::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let (fresh, long) = (Duration::ZERO, MAX_HOLD);
        assert!(o.room(t0, fresh), "nothing sent is room");
        o.push_at(video(1, false), t0);
        o.push_at(video(2, false), t0);
        assert!(!o.room(t0, long), "two frames wait in the queue");
        take(&o);
        assert!(o.room(t0, fresh), "one may wait while the next is encoded");
        take(&o);
        assert!(o.room(t0 + ms(90), fresh), "no floor yet: 100 ms");
        assert!(!o.room(t0 + ms(120), fresh));

        // Frame 1 went out alone and came back after 150 ms: the next may be out 400 ms.
        let o = Outbox::new();
        o.push_at(video(1, false), t0);
        take(&o);
        o.ack(1, ms(0), 0, t0 + ms(150));
        let t1 = t0 + ms(160);
        o.push_at(video(2, false), t1);
        take(&o);
        assert!(o.room(t1 + ms(390), fresh));
        assert!(!o.room(t1 + ms(420), fresh));
        assert!(o.room(t1 + ms(420), long));
        assert!(!o.room(t1 + ms(1200), long));
        o.push_at(video(3, false), t1 + ms(1500));
        assert_eq!(o.state.lock().unwrap().video.len(), 1, "nothing is dropped");
        take(&o);

        // A slow keyframe on the way is judged as any frame.
        let o = Outbox::new();
        o.push_at(video(1, true), t0);
        take(&o);
        assert!(!o.room(t0 + ms(900), fresh));
        assert!(o.room(t0 + ms(900), long));
        assert!(!o.room(t0 + ms(1200), long));
    }

    #[test]
    fn frames_out_and_no_ack_for_ten_seconds_is_a_stall() {
        let o = Outbox::new();
        let t0 = Instant::now();
        assert!(!o.stalled(t0 + STALL), "nothing on the way is no stall");
        o.push(video(1, true));
        take(&o);
        let sent = o.state.lock().unwrap().in_flight[0].sent;
        assert!(!o.stalled(sent + Duration::from_secs(9)));
        assert!(o.stalled(sent + STALL));
        o.ack(1, Duration::ZERO, 0, sent + Duration::from_secs(9));
        assert!(!o.stalled(sent + STALL));
    }

    #[test]
    fn a_clipboard_piece_out_and_no_ack_for_ten_seconds_is_a_stall() {
        let o = Outbox::new();
        o.push(clipboard(5, 10));
        take(&o);
        let t = Instant::now();
        assert!(o.stalled(t + STALL));
        o.ack(0, Duration::ZERO, 1, t);
        assert!(!o.stalled(t + STALL));
    }

    #[test]
    fn too_many_queued_bytes_or_a_close_refuse_the_message() {
        let o = Outbox::new();
        let big = clipboard(1, MAX_QUEUED_BYTES / 2 + 1);
        assert!(o.push(big.clone()));
        assert!(!o.push(big));
        let o = Outbox::new();
        o.close();
        assert!(!o.push(text("x")));
        assert!(matches!(o.next(Duration::ZERO), Next::Closed));
    }

    #[test]
    fn the_floor_is_the_lowest_of_the_last_one_to_two_minutes() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut f = Floor {
            start: t0,
            cur: None,
            prev: None,
        };
        f.add(ms(40), t0);
        f.add(ms(90), t0 + FLOOR_BUCKET);
        assert_eq!(f.get(), Some(ms(40)));
        f.add(ms(80), t0 + 2 * FLOOR_BUCKET);
        assert_eq!(f.get(), Some(ms(80)));
        f.add(ms(100), t0 + 5 * FLOOR_BUCKET);
        assert_eq!(f.get(), Some(ms(100)));
    }
}
