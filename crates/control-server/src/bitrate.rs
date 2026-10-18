//! Automatic bitrate (R4 in `docs/WEB-VIEWER-WAN-SCOPE.md`): one controller per monitor
//! encoder, looked at once a second on its viewers' acknowledgements.
//!
//! The encoder runs with no bitrate limit, so every frame gets QP 25 (the LAN behaviour). On a
//! full link R1 lowers the frame rate first ([`crate::outbox::Outbox::room`], W3). Only when
//! the slowest viewer of the monitor (W4) is shown fewer than [`LOW_FPS`] frames per second for
//! [`HOLD`] while the screen makes more does this limit the bitrate, to [`SHARE`] of what that
//! viewer delivered (the median of those seconds). (The frames the viewer acknowledges, not
//! those encoded: frames that pile up on the way are not shown. A second in which that viewer's
//! decoder held more than [`DECODER_HELD`] frames is the browser's, not the link's.)
//! The encoder then raises the QP up to 35 ([`media::Encoder::set_bitrate`], R5); a frame that
//! needs more is larger than its share, and R1 lowers the frame rate again. While the frame
//! rate is fine and the frames use the limit, the limit goes up step by step; at
//! [`media::encode::NO_LIMIT_KBPS`] it is no limit again. It does not go away when the screen
//! calms down: below the limit VBR gives QP 25 anyway, and each change costs a keyframe.
//!
//! Only seconds in which the screen made at least [`LOW_FPS`] frames count: a still or slow
//! screen (a caret, typing) says nothing about the link. Each change costs a keyframe with
//! `vah264enc` (it restarts the stream), so the limit changes only after [`HOLD`] of seconds of
//! one kind since the last change, by at least 20 %, and not at all while frames already
//! overshoot it (more cuts would only add keyframes).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use media::Encoder;
use media::encode::NO_LIMIT_KBPS;

/// How often a monitor is looked at.
const TICK: Duration = Duration::from_secs(1);
/// A second counts only when the screen made at least this many frames per second; then fewer
/// shown than this is a low second...
const LOW_FPS: f64 = 15.0;
/// ...and at least this many shown, or (but one) all the screen made, a clear second.
const CLEAR_FPS: f64 = 24.0;
/// This long of low or of clear seconds in a row changes the limit. Long enough that a TCP
/// loss repair on a lossy link (no acknowledgement for 1–2 s) is not taken for a slow link, and
/// at most one change (one keyframe) per this long.
const HOLD: f64 = 5.0;
/// One tick counts for at most this long: a gap in the acknowledgements (a hidden tab) is not
/// many seconds.
const MAX_TICK: f64 = 1.5;
/// A low second in which the slowest viewer's decoder held more frames than this is the
/// browser's (a slow decoder, a hidden tab), not the link's.
const DECODER_HELD: u32 = 2;
/// A new limit is this share of the delivered rate (Selkies uses 0.5–0.85).
const SHARE: f64 = 0.7;
/// A lower limit is at most this much of the one before.
const DOWN: f64 = 0.8;
/// A raise multiplies the limit by this.
const STEP: f64 = 1.25;
/// The limit goes up only while the frames use at least this share of it: below it a higher
/// limit would not change the picture.
const USED: f64 = 0.8;
/// Frames this much over their share are at the highest QP already: a lower limit would not
/// make them smaller.
const OVERSHOOT: f64 = 1.25;
const MIN_KBPS: f64 = 500.0;
/// The VA rate control gives each frame `limit / 30` (`Encoder::set_bitrate`), so what the
/// screen needs is compared at 30 frames per second.
const NOMINAL_FPS: f64 = 30.0;

/// What one tick saw.
#[derive(Default)]
struct Tick {
    secs: f64,
    /// Frames the slowest viewer acknowledged.
    shown: u32,
    /// A keyframe was on the way to a viewer: a join's or a change's keyframe holds the frames
    /// behind it back for a while on a slow link, so such a second says nothing.
    key_out: bool,
    /// The most frames the slowest viewer's decoder held, by its acknowledgements.
    decoding: u32,
    /// Frames and bytes the encoder made.
    frames: u64,
    bytes: u64,
    /// Captured frames that R1 replaced before they were encoded: with `frames`, what the screen
    /// made.
    not_encoded: u32,
    /// The slowest viewer's delivered rate, kbit/s.
    delivered: Option<f64>,
}

/// Low or clear seconds in a row, and what they saw.
#[derive(Default)]
struct Streak {
    low: bool,
    secs: f64,
    shown: u64,
    frames: u64,
    bytes: u64,
    /// What each tick delivered, kbit/s. The median: a tick with a loss repair in it delivers
    /// less than the path carries, and the one after it the burst of what waited.
    delivered: Vec<f64>,
}

#[derive(Default)]
struct Steer {
    /// kbit/s; `None` is no limit.
    cap: Option<u32>,
    streak: Streak,
}

impl Steer {
    /// The new limit and why, when it changes.
    fn tick(&mut self, t: &Tick) -> Option<(Option<u32>, String)> {
        if t.key_out {
            self.streak = Streak::default();
            return None;
        }
        let made = t.frames + t.not_encoded as u64;
        if (made as f64) < LOW_FPS * t.secs {
            return None; // a still or slow screen says nothing about the link
        }
        let fps = t.shown as f64 / t.secs;
        let low = fps < LOW_FPS;
        if low && t.decoding > DECODER_HELD {
            return None; // the browser is slow, not the link
        }
        let clear = fps >= CLEAR_FPS || made <= t.shown as u64 + 1;
        if (!low && !clear) || self.streak.low != low {
            self.streak = Streak {
                low,
                ..Streak::default()
            };
            if !low && !clear {
                return None;
            }
        }
        let s = &mut self.streak;
        s.secs += t.secs.min(MAX_TICK);
        s.shown += t.shown as u64;
        s.frames += t.frames;
        s.bytes += t.bytes;
        s.delivered.extend(t.delivered);
        if s.secs < HOLD {
            return None;
        }
        let fps = s.shown as f64 / s.secs;
        // kbit/s at 30 frames per second, to compare with the limit.
        let need = s.bytes as f64 * 8.0 / 1000.0 / s.frames.max(1) as f64 * NOMINAL_FPS;
        let (cap, why) = if low {
            let mut d = s.delivered.clone();
            d.sort_by(f64::total_cmp);
            let delivered = d.get(d.len() / 2).copied();
            if self.cap.is_some_and(|c| need > c as f64 * OVERSHOOT)
                || (self.cap.is_none() && delivered.is_none())
            {
                return None;
            }
            let mut k = delivered.map_or(f64::INFINITY, |d| d * SHARE);
            if let Some(c) = self.cap {
                k = k.min(c as f64 * DOWN);
            }
            let k = k.max(MIN_KBPS);
            if self.cap.is_some_and(|c| k >= c as f64) {
                return None;
            }
            let d = delivered.map_or("-".into(), |d| format!("{d:.0}"));
            (
                Some(k as u32),
                format!("{fps:.0} fps for {:.0} s, {d} kbit/s delivered", s.secs),
            )
        } else {
            let Some(c) = self.cap.map(f64::from) else {
                self.streak = Streak::default();
                return None;
            };
            let why = format!("{fps:.0} fps for {:.0} s, the frames use {need:.0}", s.secs);
            if need < c * USED {
                self.streak = Streak::default();
                return None;
            }
            let k = c * STEP;
            ((k < NO_LIMIT_KBPS as f64).then_some(k as u32), why)
        };
        self.cap = cap;
        self.streak = Streak::default();
        Some((cap, why))
    }
}

/// One monitor's controller and what it saw of its encoder.
pub(crate) struct Monitor {
    /// The encoder it steers: a new one (a new size) starts again with no limit.
    enc: Weak<Encoder>,
    steer: Steer,
    at: Instant,
    output: (u64, u64),
    not_encoded: u32,
}

pub(crate) type Monitors = Mutex<HashMap<u32, Monitor>>;

/// A captured frame of `monitor` was replaced before it was encoded (R1 held it back).
pub(crate) fn not_encoded(monitors: &Monitors, monitor: u32) {
    if let Some(m) = monitors.lock().unwrap().get_mut(&monitor) {
        m.not_encoded += 1;
    }
}

/// What a monitor's viewers saw since the last look.
pub(crate) struct Seen {
    /// The frames the slowest viewer acknowledged...
    pub shown: u32,
    /// ...and the most frames its decoder held.
    pub decoding: u32,
    /// A keyframe was queued or on the way to any viewer.
    pub key_out: bool,
    /// The slowest delivered rate, bytes per second.
    pub delivered: Option<f64>,
}

/// Look at `monitor` if its last look was a [`TICK`] ago, and change `enc`'s limit when the
/// controller says so.
pub(crate) fn tick(
    monitors: &Monitors,
    monitor: u32,
    enc: &Arc<Encoder>,
    viewers: impl FnOnce() -> Seen,
) {
    let now = Instant::now();
    let change = {
        let mut map = monitors.lock().unwrap();
        let m = map.entry(monitor).or_insert_with(|| fresh(enc, now));
        if !std::ptr::eq(m.enc.as_ptr(), Arc::as_ptr(enc)) {
            *m = fresh(enc, now);
        }
        let secs = now.saturating_duration_since(m.at).as_secs_f64();
        if secs < TICK.as_secs_f64() {
            return;
        }
        let output = enc.output();
        let seen = viewers();
        let t = Tick {
            secs,
            shown: seen.shown,
            key_out: seen.key_out,
            decoding: seen.decoding,
            frames: output.0 - m.output.0,
            bytes: output.1 - m.output.1,
            not_encoded: std::mem::take(&mut m.not_encoded),
            delivered: seen.delivered.map(|b| b * 8.0 / 1000.0),
        };
        (m.at, m.output) = (now, output);
        let old = m.steer.cap;
        m.steer.tick(&t).map(|(cap, why)| (old, cap, why))
    };
    // Outside the lock: the encoder takes the change at its next frame.
    if let Some((old, cap, why)) = change {
        let show = |c: Option<u32>| c.map_or("no limit".into(), |k| format!("{k} kbit/s"));
        tracing::info!(
            "monitor {monitor} bitrate: {} → {} ({why})",
            show(old),
            show(cap)
        );
        enc.set_bitrate(cap);
    }
}

fn fresh(enc: &Arc<Encoder>, now: Instant) -> Monitor {
    Monitor {
        enc: Arc::downgrade(enc),
        steer: Steer::default(),
        at: now,
        output: enc.output(),
        not_encoded: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second: `fps` frames of `kb` KB each encoded and shown, `held` frames not encoded,
    /// `delivered` kbit/s.
    fn sec(fps: u64, kb: u64, held: u32, delivered: f64) -> Tick {
        Tick {
            secs: 1.0,
            shown: fps as u32,
            frames: fps,
            bytes: fps * kb * 1000,
            not_encoded: held,
            delivered: Some(delivered),
            ..Tick::default()
        }
    }

    /// Run `n` ticks of `t`; the changes, in order.
    fn run(s: &mut Steer, n: u32, t: &Tick) -> Vec<Option<u32>> {
        (0..n)
            .filter_map(|_| s.tick(t))
            .map(|(cap, _)| cap)
            .collect()
    }

    /// The frame rate falls first; only 5 s under 15 fps shown while the screen makes more
    /// limits the bitrate, to 70 % of the median delivered in those seconds (one may have a loss
    /// repair in it, the next the burst after it). A slow screen, or a still one, says nothing.
    #[test]
    fn a_low_frame_rate_for_five_seconds_limits_the_bitrate() {
        let mut s = Steer::default();
        assert!(
            run(&mut s, 10, &sec(8, 20, 0, 1300.0)).is_empty(),
            "slow screen"
        );
        assert!(
            run(&mut s, 10, &sec(50, 4, 5, 1600.0)).is_empty(),
            "fast enough"
        );
        assert!(run(&mut s, 3, &sec(10, 20, 30, 2000.0)).is_empty());
        assert!(
            run(&mut s, 1, &sec(10, 20, 30, 500.0)).is_empty(),
            "a repair"
        );
        assert!(run(&mut s, 1, &sec(0, 0, 0, 0.0)).is_empty(), "still");
        assert_eq!(run(&mut s, 1, &sec(10, 20, 30, 5000.0)), vec![Some(1400)]);
    }

    /// Frames that pile up on the way, with none held back, are not shown either.
    #[test]
    fn frames_that_pile_up_on_the_way_are_a_low_frame_rate() {
        let mut s = Steer::default();
        let piling = Tick {
            shown: 6,
            ..sec(40, 20, 0, 1000.0)
        };
        assert_eq!(run(&mut s, 5, &piling), vec![Some(700)]);
    }

    /// A second with a keyframe on the way ends a streak: a join's keyframe holds the frames
    /// behind it back on a slow link.
    #[test]
    fn a_keyframe_on_the_way_ends_a_streak() {
        let mut s = Steer::default();
        let low = sec(5, 20, 30, 1000.0);
        assert!(run(&mut s, 4, &low).is_empty());
        let key = Tick {
            key_out: true,
            ..sec(5, 20, 30, 1000.0)
        };
        assert!(run(&mut s, 1, &key).is_empty());
        assert!(run(&mut s, 4, &low).is_empty());
        assert_eq!(run(&mut s, 1, &low), vec![Some(700)]);
    }

    /// A low second in which the decoder held more than two frames is the browser's: a slow
    /// decoder or a hidden tab on the LAN does not limit the bitrate.
    #[test]
    fn a_slow_decoder_is_not_a_slow_link() {
        let mut s = Steer::default();
        let decoding = Tick {
            decoding: 3,
            ..sec(5, 20, 30, 1000.0)
        };
        assert!(run(&mut s, 30, &decoding).is_empty());
        let two = Tick {
            decoding: 2,
            ..decoding
        };
        assert_eq!(run(&mut s, 5, &two), vec![Some(700)]);
    }

    /// A tick after a long gap in the acknowledgements counts as 1.5 s, not as many seconds.
    #[test]
    fn a_long_tick_counts_as_one_and_a_half_seconds() {
        let mut s = Steer::default();
        let gap = Tick {
            secs: 30.0,
            shown: 30,
            frames: 30,
            bytes: 30 * 20_000,
            not_encoded: 900,
            delivered: Some(1000.0),
            ..Tick::default()
        };
        assert!(run(&mut s, 1, &gap).is_empty());
        assert!(run(&mut s, 3, &sec(5, 20, 30, 1000.0)).is_empty());
        assert_eq!(run(&mut s, 1, &sec(5, 20, 30, 1000.0)), vec![Some(700)]);
    }

    /// At most one change per 5 s, each one at least 20 % lower; none while frames overshoot
    /// the limit (they are at the highest QP already).
    #[test]
    fn cuts_are_spaced_and_stop_when_frames_overshoot() {
        let mut s = Steer::default();
        assert_eq!(run(&mut s, 5, &sec(10, 20, 30, 2000.0)), vec![Some(1400)]);
        // 1400 kbit/s is 5.8 KB per frame. 5 KB frames, still low: cut again, to 80 % since
        // 70 % of 1800 is not lower.
        assert_eq!(run(&mut s, 5, &sec(12, 5, 30, 1800.0)), vec![Some(1120)]);
        // 20 KB frames are far over 1120 / 30: no more cuts.
        assert!(run(&mut s, 20, &sec(5, 20, 30, 800.0)).is_empty());
    }

    /// A clear link raises the limit by 1.25 every 5 s while the frames use at least 80 % of
    /// it. A calm screen keeps the limit: it is not removed.
    #[test]
    fn a_clear_link_raises_the_limit_while_the_frames_use_it() {
        let mut s = Steer::default();
        assert_eq!(run(&mut s, 5, &sec(10, 20, 30, 1000.0)), vec![Some(700)]);
        // 4 KB frames at 30 fps use 960 kbit/s.
        let busy = sec(30, 4, 2, 700.0);
        assert_eq!(
            run(&mut s, 30, &busy),
            vec![Some(875), Some(1093), Some(1366)]
        );
        // A caret or typing: under 15 frames per second made.
        assert!(run(&mut s, 30, &sec(2, 1, 0, 50.0)).is_empty());
        // Small frames: 1 KB at 30 fps is 240 kbit/s.
        assert!(run(&mut s, 30, &sec(40, 1, 0, 400.0)).is_empty());
        assert_eq!(s.cap, Some(1366));
    }

    /// A raise that reaches 50 Mbit/s is no limit.
    #[test]
    fn a_limit_that_reaches_50_mbit_s_is_no_limit() {
        let mut s = Steer {
            cap: Some(42_000),
            ..Steer::default()
        };
        // 150 KB frames at 30 fps use 36 Mbit/s.
        assert_eq!(run(&mut s, 5, &sec(30, 150, 0, 40_000.0)), vec![None]);
        assert!(run(&mut s, 30, &sec(30, 150, 0, 40_000.0)).is_empty());
    }
}
