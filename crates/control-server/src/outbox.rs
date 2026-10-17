//! One browser viewer's send queue, and what the browser's acknowledgements say about its path.
//!
//! - **Two lanes.** Control messages (every text message, and clipboard bytes) go before video,
//!   so a cursor move or an `echo` never waits behind queued frames. The writer
//!   ([`crate::webviewer`]) takes from [`Outbox::next`], which stamps each video frame as it
//!   hands it to the socket.
//! - **Acknowledgements.** The browser sends the newest frame it received and how long it held
//!   that number ([`wire::browser::FromBrowser::Ack`]). Each one gives a round trip (from the
//!   stamp, less the hold) and a floor (the lowest round trip of the last 1–2 minutes), and
//!   ends the frames it covers.
//! - **Behind** ([`State::behind`]): how long the oldest video frame that is not acknowledged
//!   (queued here or on the way) has waited since it was queued, less the floor. One number
//!   for our queue, the kernel's, any proxy's and the path's.
//! - **Resync.** A viewer more than [`BEHIND_MAX`] behind loses its queued video. Frames already
//!   handed to the socket cannot be taken back, so it then gets no video until those are
//!   acknowledged, and then asks for a keyframe ([`Outbox::ack`] returns true). Deltas are
//!   dropped until a keyframe comes, and the delay is not judged again until that keyframe is
//!   acknowledged, so one slow keyframe does not start the next resync.
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
/// cannot hold more memory. Video stays under about one second (the resync); this is for the
/// control lane: two of the largest messages (a 32 MiB clipboard image).
const MAX_QUEUED_BYTES: usize = 2 * wire::socket::chunk::MAX_MESSAGE_BYTES;
/// The floor is the lowest round trip of this bucket and the one before, so of the last 1 to
/// 2 minutes. Long enough that a queue which stands for a while (a scroll on a slow link) does
/// not lift it, short enough to follow a new route (Wi-Fi to a phone) within two minutes.
const FLOOR_BUCKET: Duration = Duration::from_secs(60);
/// An acknowledgement held longer than its frame has been out names an earlier frame with the
/// same number (a new encoder counts from 1 again). This much is rounding.
const HELD_SLACK: Duration = Duration::from_millis(5);
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
    /// The delay is judged only from here: when the last resync's keyframe was acknowledged.
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Live,
    /// Too far behind: drop video until a keyframe.
    Behind,
    /// The resync keyframe `seq` is queued or on the way; the delay is not judged until it is
    /// acknowledged.
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

    /// Start a resync if the viewer is live and too far behind: drop its queued video.
    fn judge(&mut self, now: Instant) {
        if self.mode != Mode::Live || self.behind(now) <= BEHIND_MAX {
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
                if st.mode == Mode::Behind {
                    if !key {
                        st.period.dropped += 1;
                        return true;
                    }
                    st.mode = Mode::KeyOut(seq);
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
                let Queued::Video { seq, .. } = msg else {
                    unreachable!("the video lane holds video")
                };
                let now = Instant::now();
                if st.in_flight.is_empty() {
                    st.progress_at = now;
                }
                st.in_flight.push_back(Sent {
                    seq,
                    len: n,
                    queued,
                    sent: now,
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
        // Each frame is acknowledged once, so the first match is the frame (an older frame
        // with the same number would have been acknowledged before it).
        if let Some(i) = st.in_flight.iter().position(|f| f.seq == seq) {
            let out = now.saturating_duration_since(st.in_flight[i].sent);
            if held <= out + HELD_SLACK {
                let rtt = out.saturating_sub(held);
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
                st.floor.add(rtt, now);
                st.period.rtt_sum += rtt;
                st.period.rtts += 1;
                st.judge(now);
            }
        }
        st.mode == Mode::Behind && st.in_flight.is_empty()
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
        let p = std::mem::replace(&mut st.period, Period::new(now));
        if p.acked_bytes == 0 && p.dropped == 0 && st.in_flight.is_empty() && st.video.is_empty() {
            return None;
        }
        let ms = |d: Duration| format!("{:.0}", d.as_secs_f64() * 1000.0);
        let rtt = match p.rtts {
            0 => "-".into(),
            n => ms(p.rtt_sum / n),
        };
        let in_flight_bytes: usize = st.in_flight.iter().map(|f| f.len).sum();
        Some(format!(
            "rtt {rtt} ms, floor {} ms, behind {} ms, in flight {} frames {} KB, queued {} \
             frames, {:.2} Mbit/s delivered, {} dropped, {} resyncs",
            st.floor.get().map_or("-".into(), ms),
            ms(st.behind(now)),
            st.in_flight.len(),
            in_flight_bytes / 1000,
            st.video.len(),
            p.acked_bytes as f64 * 8.0 / secs / 1e6,
            p.dropped,
            p.resyncs,
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
            o.push(video(s, s == 1));
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
        o.push_at(video(1, true), t0);
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
