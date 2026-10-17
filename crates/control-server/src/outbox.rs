//! One browser viewer's send queue, and what the browser's acknowledgements say about its path.
//!
//! - **Two lanes.** Control messages (every text message, and clipboard bytes) go before video,
//!   so a cursor move or an `echo` never waits behind queued frames. The writer
//!   ([`crate::webviewer`]) takes from [`Outbox::next`], which stamps each video frame as it
//!   hands it to the socket.
//! - **Acknowledgements.** The browser sends the newest frame it received and how long it held
//!   that number ([`wire::browser::FromBrowser::Ack`]). Each one gives a round trip (from the
//!   stamp, less the hold) and a floor (the lowest round trip of the last 1–2 minutes, of
//!   deltas sent while no keyframe was on the way), and ends the frames it covers.
//! - **Behind** ([`State::behind`]): how long the oldest video frame that is not acknowledged
//!   (queued here or on the way) has waited since it was queued, less the floor. One number
//!   for our queue, the kernel's, any proxy's and the path's.
//! - **Resync.** A viewer more than [`BEHIND_MAX`] behind with video queued here loses that
//!   video (with nothing queued, [`Outbox::room`] already holds new frames back). Frames already
//!   handed to the socket cannot be taken back, so it then gets no video until those are
//!   acknowledged, and then asks for a keyframe ([`Outbox::ack`] returns true). Deltas are
//!   dropped until a keyframe comes.
//! - **Keyframes** (resync, join, selection change, request): the delay is not judged from when
//!   one is handed to the socket until it is acknowledged, so one slow keyframe does not start
//!   a resync. While it still waits in our queue it is judged as usual.
//! - **Room** ([`Outbox::room`]): whether the encoder may make another frame for this viewer
//!   (R1 in `docs/WEB-VIEWER-WAN-SCOPE.md`; the media plane holds the newest captured frame
//!   until every viewer of its monitor has room). While a keyframe is on the way, nothing is
//!   held: the deltas behind it stay small, and the video lane is bounded by the time the
//!   keyframe takes to be acknowledged ([`STALL`] at most).
//! - **Stall** ([`Outbox::stalled`]): frames on the way and no acknowledgement that moves for
//!   [`STALL`]. The socket is closed only then, whatever one write takes.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::mediaplane::Queued;

/// A viewer further behind than this is resynced.
const BEHIND_MAX: Duration = Duration::from_secs(1);
/// Frames on the way and no acknowledgement that moves for this long: the viewer is stuck.
const STALL: Duration = Duration::from_secs(10);
/// More bytes than this queued in both lanes closes the viewer, so one that stops reading
/// cannot hold more memory. Video stays under about one second (the resync), or the time a
/// keyframe on the way takes to be acknowledged; this is for the control lane: two of the
/// largest messages (a 32 MiB clipboard image).
const MAX_QUEUED_BYTES: usize = 2 * wire::socket::chunk::MAX_MESSAGE_BYTES;
/// The floor is the lowest round trip of this bucket and the one before, so of the last 1 to
/// 2 minutes. Long enough that a queue which stands for a while (a scroll on a slow link) does
/// not lift it, short enough to follow a new route (Wi-Fi to a phone) within two minutes.
const FLOOR_BUCKET: Duration = Duration::from_secs(60);
/// An acknowledgement held longer than its frame has been out names an earlier frame with the
/// same number (a new encoder counts from 1 again). This much is rounding.
const HELD_SLACK: Duration = Duration::from_millis(5);
/// A viewer has room for another frame while it is at most this plus one floor behind
/// ([`State::behind`]): its oldest frame not acknowledged has been out at most two round trips
/// and this. Acknowledgements come up to 50 ms late (`ACK_MS` in `viewer/worker.ts`), and the
/// other 50 ms keeps a link busy across one of them. The extra round trip is for loss: TCP
/// needs about one round trip to repair a lost packet, and without it a 1 % loss link held
/// frames back at every repair.
const ROOM: Duration = Duration::from_millis(100);
/// While a viewer is less than [`BEHIND_MAX`] behind, a frame waits for room at most this
/// long: at least 10 frames per second. A held frame covers more motion when it is encoded,
/// and on scrolling text a step twice as long costs more than twice the bytes (the encoder
/// follows only short motion). Held without a limit, the frames grew until a 3 Mbit/s link
/// carried 2–3 frames per second at a higher bitrate than 50 frames per second need
/// (docs/WAN-BENCH.md, round 3). For the same reason nothing is held while a keyframe is on
/// the way, and the floor leaves out keyframes and the deltas behind them.
const MAX_HOLD: Duration = Duration::from_millis(100);
/// The debug summary comes at most this often.
const LOG_EVERY: Duration = Duration::from_secs(5);

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
    /// Video handed to the socket and not yet acknowledged, oldest first.
    in_flight: VecDeque<Sent>,
    queued_bytes: usize,
    closed: bool,
    mode: Mode,
    /// The delay is judged only from here: when the last keyframe was acknowledged.
    judge_from: Option<Instant>,
    /// When an acknowledgement last moved, or a frame went out with none on the way.
    progress_at: Instant,
    floor: Floor,
    period: Period,
}

#[derive(Clone, Copy)]
struct Sent {
    seq: u32,
    len: usize,
    queued: Instant,
    sent: Instant,
    /// A delta handed to the socket while no keyframe was on the way. Only its round trip
    /// counts for the floor: a keyframe's, and those of the deltas behind it, are longer than
    /// the path's, and every join starts with a keyframe.
    for_floor: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Live,
    /// Too far behind: drop video until a keyframe.
    Behind,
    /// The keyframe `seq` is queued or on the way. Once it is on the way, the delay is not
    /// judged until it is acknowledged.
    KeyOut(u32),
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
    dropped: u32,
    resyncs: u32,
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
            dropped: 0,
            resyncs: 0,
            not_encoded: 0,
            delays: Vec::new(),
        }
    }
}

fn len(msg: &Queued) -> usize {
    match msg {
        Queued::Video { bytes, .. } | Queued::Binary(bytes) => bytes.len(),
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
        let from = self.judge_from.map_or(oldest, |j| oldest.max(j));
        now.saturating_duration_since(from)
            .saturating_sub(self.floor.get().unwrap_or_default())
    }

    /// Start a resync if the viewer is judged, too far behind, and video waits in our queue:
    /// drop that video, also a keyframe that still waits there (never only the deltas behind
    /// it). With nothing queued here a resync cannot help: what is on the way stays on the way,
    /// and [`Outbox::room`] already holds new frames back, so the next frame can be a delta
    /// instead of a keyframe.
    fn judge(&mut self, now: Instant) {
        let judged = match self.mode {
            Mode::Live => true,
            Mode::KeyOut(k) => self
                .video
                .iter()
                .any(|(m, _)| matches!(m, Queued::Video { seq, .. } if *seq == k)),
            Mode::Behind => false,
        };
        if !judged || self.video.is_empty() || self.behind(now) <= BEHIND_MAX {
            return;
        }
        self.period.dropped += self.video.len() as u32;
        let dropped: usize = self.video.drain(..).map(|(m, _)| len(&m)).sum();
        self.queued_bytes -= dropped;
        self.period.resyncs += 1;
        self.mode = Mode::Behind;
    }
}

impl Outbox {
    pub(crate) fn new() -> Outbox {
        let now = Instant::now();
        Outbox {
            state: Mutex::new(State {
                control: VecDeque::new(),
                video: VecDeque::new(),
                in_flight: VecDeque::new(),
                queued_bytes: 0,
                closed: false,
                mode: Mode::Live,
                judge_from: None,
                progress_at: now,
                floor: Floor {
                    start: now,
                    cur: None,
                    prev: None,
                },
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
        let mut st = self.state.lock().unwrap();
        if st.closed {
            return false;
        }
        let n = len(&msg);
        match msg {
            Queued::Video { seq, key, .. } => {
                st.judge(now);
                if key {
                    st.mode = Mode::KeyOut(seq);
                } else if st.mode == Mode::Behind {
                    st.period.dropped += 1;
                    return true;
                }
                st.video.push_back((msg, now));
            }
            _ => st.control.push_back(msg),
        }
        st.queued_bytes += n;
        if st.queued_bytes > MAX_QUEUED_BYTES {
            return false;
        }
        self.ready.notify_one();
        true
    }

    /// The next message to send, control first. Waits up to `timeout`. A video frame counts as
    /// handed to the socket from here.
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
            if let Some((msg, queued)) = st.video.pop_front() {
                let n = len(&msg);
                st.queued_bytes -= n;
                let Queued::Video { seq, key, .. } = msg else {
                    unreachable!("the video lane holds video")
                };
                let now = Instant::now();
                if st.in_flight.is_empty() {
                    st.progress_at = now;
                }
                let for_floor = !key && !matches!(st.mode, Mode::KeyOut(_));
                st.in_flight.push_back(Sent {
                    seq,
                    len: n,
                    queued,
                    sent: now,
                    for_floor,
                });
                return Next::Msg(msg);
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return Next::Idle;
            }
            st = self.ready.wait_timeout(st, wait).unwrap().0;
        }
    }

    /// Drop everything and stop the writer, also inside a send.
    pub(crate) fn close(&self) {
        let mut st = self.state.lock().unwrap();
        st.closed = true;
        st.control.clear();
        st.video.clear();
        st.queued_bytes = 0;
        self.ready.notify_all();
        // Stores a permit when the writer is not waiting yet.
        self.closing.notify_one();
    }

    /// Completes when [`Outbox::close`] was called. The writer sends inside a `select!` on it.
    pub(crate) async fn closed(&self) {
        self.closing.notified().await
    }

    /// The browser received every frame up to `seq` and held that number for `held`. True when
    /// the viewer waits to resync and has caught up: the caller asks for a keyframe.
    pub(crate) fn ack(&self, seq: u32, held: Duration, now: Instant) -> bool {
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
                let mut key_acked = false;
                for f in st.in_flight.drain(..=i).collect::<Vec<_>>() {
                    st.period.acked_bytes += f.len;
                    key_acked |= st.mode == Mode::KeyOut(f.seq);
                }
                if key_acked {
                    st.mode = Mode::Live;
                    st.judge_from = Some(now);
                }
                st.progress_at = now;
                if for_floor {
                    st.floor.add(rtt, now);
                }
                st.period.rtt_sum += rtt;
                st.period.rtts += 1;
                st.judge(now);
            }
        }
        st.mode == Mode::Behind && st.in_flight.is_empty()
    }

    /// Whether the encoder may make another frame for this viewer, when a frame has waited
    /// `held` for room: at most one frame waits in our queue (the socket writes it while the
    /// next one is encoded; a frame larger than the kernel's 128 KiB waits there while it is
    /// written), and it is at most [`ROOM`] plus the floor behind, or less than [`BEHIND_MAX`]
    /// behind once the frame has waited [`MAX_HOLD`]. A viewer that waits for its resync
    /// drops deltas anyway, and one with a keyframe on the way holds nothing back (see the
    /// module docs). A viewer that stops acknowledging holds the others back until [`STALL`]
    /// closes it.
    pub(crate) fn room(&self, now: Instant, held: Duration) -> bool {
        let st = self.state.lock().unwrap();
        let behind = st.behind(now);
        matches!(st.mode, Mode::Behind | Mode::KeyOut(_))
            || (st.video.len() <= 1
                && (behind <= ROOM + st.floor.get().unwrap_or_default()
                    || (held >= MAX_HOLD && behind <= BEHIND_MAX)))
    }

    /// A captured frame of this viewer's monitor was replaced before it was encoded.
    pub(crate) fn not_encoded(&self) {
        self.state.lock().unwrap().period.not_encoded += 1;
    }

    /// Frames are on the way and no acknowledgement has moved for [`STALL`].
    pub(crate) fn stalled(&self, now: Instant) -> bool {
        let st = self.state.lock().unwrap();
        !st.in_flight.is_empty() && now.saturating_duration_since(st.progress_at) >= STALL
    }

    /// The summary for the debug log, at most once per [`LOG_EVERY`], and only when video
    /// moved or waits.
    pub(crate) fn summary(&self, now: Instant) -> Option<String> {
        let mut st = self.state.lock().unwrap();
        let secs = now.saturating_duration_since(st.period.start).as_secs_f64();
        if secs < LOG_EVERY.as_secs_f64() {
            return None;
        }
        let mut p = std::mem::replace(&mut st.period, Period::new(now));
        if p.acked_bytes == 0 && p.dropped == 0 && st.in_flight.is_empty() && st.video.is_empty() {
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
             frames {} KB, queued {} frames, {:.2} Mbit/s delivered, {} dropped, {} resyncs, {} \
             not encoded",
            pct(0.5),
            pct(0.95),
            st.floor.get().map_or("-".into(), ms),
            ms(st.behind(now)),
            st.in_flight.len(),
            in_flight_bytes / 1000,
            st.video.len(),
            p.acked_bytes as f64 * 8.0 / secs / 1e6,
            p.dropped,
            p.resyncs,
            p.not_encoded,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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

    /// The writer's next message, which must be there already.
    fn take(o: &Outbox) -> Queued {
        match o.next(Duration::ZERO) {
            Next::Msg(m) => m,
            _ => panic!("nothing queued"),
        }
    }

    fn seq(m: &Queued) -> u32 {
        match m {
            Queued::Video { seq, .. } => *seq,
            _ => panic!("not video: {m:?}"),
        }
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
        assert!(!o.ack(2, ms(10), sent + ms(60)));
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
        o.ack(1, ms(0), sent + ms(400));
        o.ack(2, ms(0), sent + ms(450));
        assert_eq!(o.state.lock().unwrap().floor.get(), None);
        o.push(video(3, false));
        take(&o);
        let sent = o.state.lock().unwrap().in_flight[0].sent;
        o.ack(3, ms(0), sent + ms(150));
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
        o.ack(1, Duration::from_secs(5), sent + Duration::from_millis(20));
        let st = o.state.lock().unwrap();
        assert_eq!(st.in_flight.len(), 1);
        assert_eq!(st.floor.get(), None);
    }

    /// More than a second behind: the queued video goes, deltas are dropped, and once the
    /// frames on the way are acknowledged the viewer asks for a keyframe. The keyframe is not
    /// judged while it crosses; after its acknowledgement the clock starts again.
    #[test]
    fn a_viewer_far_behind_drops_its_video_and_resyncs_on_a_keyframe() {
        let o = Outbox::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        o.push_at(video(1, false), t0);
        take(&o); // on the way
        o.push_at(video(2, false), t0 + ms(10)); // queued
        // 1.2 s later frame 1 is still not acknowledged: the next frame starts a resync.
        assert!(o.push_at(video(3, false), t0 + ms(1200)));
        {
            let st = o.state.lock().unwrap();
            assert_eq!(st.mode, Mode::Behind);
            assert!(st.video.is_empty(), "the queued video was not dropped");
            assert_eq!(st.period.dropped, 2);
        }
        // Frame 1 arrives: caught up, so ask for a keyframe; deltas still go.
        let sent = o.state.lock().unwrap().in_flight[0].sent;
        assert!(o.ack(1, ms(0), sent + ms(1300)));
        o.push_at(video(4, false), t0 + ms(1310));
        assert!(o.state.lock().unwrap().video.is_empty());
        // The keyframe is taken and not judged while it crosses, however long that is.
        o.push_at(video(5, true), t0 + ms(1400));
        take(&o);
        o.push_at(video(6, false), t0 + ms(1410));
        o.push_at(video(7, false), t0 + ms(4000));
        assert_eq!(o.state.lock().unwrap().video.len(), 2);
        // Acknowledged: live again, and the frames behind it are judged from now.
        let sent = o.state.lock().unwrap().in_flight[0].sent;
        assert!(!o.ack(5, ms(0), sent + ms(10)));
        let st = o.state.lock().unwrap();
        assert_eq!(st.mode, Mode::Live);
        assert_eq!(st.video.len(), 2);
    }

    /// Any keyframe (a join, a request) is not judged while it crosses, so a slow one does not
    /// drop the deltas behind it and cause a second keyframe.
    #[test]
    fn a_keyframe_on_the_way_is_not_judged() {
        let o = Outbox::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        o.push_at(video(1, true), t0);
        take(&o);
        assert!(o.push_at(video(2, false), t0 + ms(1500)));
        let st = o.state.lock().unwrap();
        assert_eq!(st.mode, Mode::KeyOut(1));
        assert_eq!(st.video.len(), 1);
    }

    /// A keyframe that still waits in our queue (behind a large clipboard message) is judged:
    /// the whole lane goes, keyframe included, so deltas cannot pile up behind it.
    #[test]
    fn a_keyframe_still_queued_is_judged_and_dropped_with_its_lane() {
        let o = Outbox::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        o.push_at(video(1, true), t0);
        o.push_at(video(2, false), t0 + ms(10));
        assert!(o.push_at(video(3, false), t0 + ms(1200)));
        let st = o.state.lock().unwrap();
        assert_eq!(st.mode, Mode::Behind);
        assert!(st.video.is_empty());
        assert_eq!(st.period.dropped, 3);
    }

    /// Room: at most one frame queued here, and the oldest frame on the way out at most two
    /// floors and `ROOM`; or, for a frame held `MAX_HOLD`, less than `BEHIND_MAX` behind; or a
    /// keyframe on the way. Far behind with nothing queued is no resync: the next frame can be
    /// a delta.
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
        o.ack(1, ms(0), t0 + ms(150));
        let t1 = t0 + ms(160);
        o.push_at(video(2, false), t1);
        take(&o);
        assert!(o.room(t1 + ms(390), fresh));
        assert!(!o.room(t1 + ms(420), fresh));
        assert!(o.room(t1 + ms(420), long));
        assert!(!o.room(t1 + ms(1200), long));
        assert!(o.push_at(video(3, false), t1 + ms(1500)));
        assert_eq!(o.state.lock().unwrap().mode, Mode::Live);
        take(&o);
        o.push_at(video(4, true), t1 + ms(1500));
        take(&o);
        assert!(
            o.room(t1 + ms(3000), fresh),
            "a keyframe on the way holds nothing back"
        );
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
        o.ack(1, Duration::ZERO, sent + Duration::from_secs(9));
        assert!(!o.stalled(sent + STALL));
    }

    #[test]
    fn too_many_queued_bytes_or_a_close_refuse_the_message() {
        let o = Outbox::new();
        let big = Queued::Binary(Arc::from(vec![0u8; MAX_QUEUED_BYTES / 2 + 1]));
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
