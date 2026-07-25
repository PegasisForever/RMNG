//! Viewer-side audio: play the selected clone's desktop sound, capture the operator's mic.
//!
//! Both pipelines use `autoaudiosink`/`autoaudiosrc`, which resolve to
//! `pulsesink`/`pipewiresrc` on Linux and `osxaudiosink`/`osxaudiosrc` on macOS — so unlike
//! the video path (where `vah264dec` vs `vtdec_hw` forces a `cfg` split) one description
//! covers both platforms.
//!
//! Opus is decoded/encoded here rather than at the control-server: the server relays the
//! frames opaquely, which keeps every audio GStreamer element out of the media plane and
//! leaves its plugin-scan boot ordering untouched.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

/// Matches the clone's capture rate and Opus's native rate — no resampling in the common path.
const RATE: i32 = 48_000;
/// The clone captures its desktop in stereo (the mic goes back mono; that rate/layout is
/// pinned in `MIC_DESC`'s caps filter).
const OUT_CHANNELS: i32 = 2;

/// Playback: server Opus → the operator's speakers.
///
/// No `opusparse` — each buffer we push is exactly one encoder frame, because the port-1
/// framing preserved the boundary. `opusparse` is for recovering boundaries from a flat
/// byte stream and would collapse pre-framed input.
///
/// **`sync=true`, unlike the video sink**, and that difference is load-bearing rather than
/// an inconsistency. The clone captures on its own sound card's crystal; these speakers run
/// on a different one, and they differ by tens of ppm. Nothing reconciles two clocks for
/// free: a synchronising audio sink slaves itself to the incoming stream (`pulsesink`'s
/// `slave-method`, "skew" by default), which is the only thing here that can absorb that
/// drift. With `sync=false` that machinery is disabled, so the difference accumulates in the
/// queue below until it saturates and starts leaking — and because a leak here drops an
/// *encoded* frame, it desyncs `opusdec` and turns into audible noise. That failure takes
/// minutes to appear, which is exactly what makes it worth a comment: it is invisible in a
/// short test and unmistakable in real use.
///
/// The cost is one buffer of latency instead of zero. Audio has no A/V sync to maintain
/// (the video path carries no PTS discipline to sync against), so this buys correctness
/// with a delay nobody can hear.
///
/// The queue stays `leaky=downstream` as a last-resort backstop for a genuine stall, but
/// with the sink slaving it should never reach that bound in steady state.
const PLAY_DESC: &str = "\
    appsrc name=src is-live=true format=time do-timestamp=true \
      max-buffers=32 leaky-type=downstream ! \
    opusdec plc=true ! \
    audioconvert ! audioresample ! \
    queue max-size-time=100000000 leaky=downstream ! \
    autoaudiosink name=sink sync=true";

/// Capture: operator microphone → Opus.
///
/// `audio-type=voice` and a low bitrate because this is speech, not music; mono halves it
/// again. `inband-fec` lets the decoder reconstruct a lost frame from the next one.
const MIC_DESC: &str = "\
    autoaudiosrc name=src ! \
    audioconvert ! audioresample ! \
    audio/x-raw,format=S16LE,rate=48000,channels=1 ! \
    queue max-size-buffers=8 leaky=downstream ! \
    opusenc bitrate=32000 frame-size=20 audio-type=voice inband-fec=true ! \
    appsink name=out emit-signals=true max-buffers=8 sync=false drop=true";

/// Speaker: decodes and plays whatever the server relays.
pub struct Playback {
    pipeline: gst::Pipeline,
    src: AppSrc,
    /// Local mute. Muting stops feeding the appsrc rather than unsubscribing server-side,
    /// so unmute is instant (the bandwidth it wastes is ~12 KB/s).
    muted: AtomicBool,
    last_seq: AtomicU64,
}

impl Drop for Playback {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Playback {
    pub fn start() -> Result<Self> {
        let pipeline = gst::parse::launch(PLAY_DESC)
            .context("build audio playback pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("playback pipeline is not a Pipeline"))?;
        let src = pipeline
            .by_name("src")
            .context("appsrc 'src' missing")?
            .downcast::<AppSrc>()
            .map_err(|_| anyhow!("'src' is not an AppSrc"))?;
        // Raw Opus frames, no container: `opusdec` can't infer these without a header page.
        src.set_caps(Some(
            &gst::Caps::builder("audio/x-opus")
                .field("channel-mapping-family", 0i32)
                .field("rate", RATE)
                .field("channels", OUT_CHANNELS)
                .build(),
        ));
        pipeline.set_state(gst::State::Playing).context("start audio playback")?;
        Ok(Self {
            pipeline,
            src,
            // Honour a mute pressed before the first frame built this pipeline.
            muted: AtomicBool::new(SPEAKER_MUTED.load(Ordering::Relaxed)),
            last_seq: AtomicU64::new(u64::MAX),
        })
    }

    /// Feed one Opus frame from the server.
    pub fn push(&self, seq: u64, opus: Vec<u8>) {
        if self.muted.load(Ordering::Relaxed) {
            return;
        }
        let prev = self.last_seq.swap(seq, Ordering::Relaxed);
        if prev != u64::MAX && seq > prev.wrapping_add(1) {
            tracing::debug!("audio gap: {} frame(s) lost", seq - prev - 1);
        }
        if let Err(e) = self.src.push_buffer(gst::Buffer::from_mut_slice(opus)) {
            tracing::debug!("audio push failed: {e:?}");
        }
    }

    /// Selection changed: drop the decoder's state. Opus is stateful, so frames from the
    /// newly-selected clone must not be decoded against the previous clone's history.
    ///
    /// The flush is sent to the **pipeline**, not to the appsrc. `Element::send_event` on a
    /// source hands the event to that element alone; only the pipeline routes a flush
    /// downstream through `opusdec` and the queue, which is where the stale state actually
    /// lives. Flushing resets the running time, so `FlushStop(reset_time: true)` is what lets
    /// `do-timestamp` start the new clone's stream from zero rather than resuming mid-timeline.
    pub fn reset(&self) {
        self.last_seq.store(u64::MAX, Ordering::Relaxed);
        // Cheaper and less disruptive than tearing the pipeline down, and it drops exactly
        // the stale buffers we want gone.
        let _ = self.pipeline.send_event(gst::event::FlushStart::new());
        let _ = self.pipeline.send_event(gst::event::FlushStop::new(true));
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
}

/// Microphone: encodes the operator's mic and hands frames to a callback for upload.
pub struct Mic {
    pipeline: gst::Pipeline,
    /// Shared with the appsink callback, which reads it to decide whether to emit a frame.
    /// Must be the *same* allocation the callback holds, not a copy.
    muted: std::sync::Arc<AtomicBool>,
}

impl Drop for Mic {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Mic {
    /// Open the microphone. `on_frame` is called from a GStreamer streaming thread for
    /// every encoded frame.
    ///
    /// Returns `Err` when no capture device exists — a perfectly normal operator setup, so
    /// the caller disables the button rather than treating it as fatal. (Contrast the video
    /// decoder, where a missing element genuinely is unrecoverable.)
    pub fn start<F>(mut on_frame: F) -> Result<Self>
    where
        F: FnMut(Vec<u8>) + Send + 'static,
    {
        let pipeline = gst::parse::launch(MIC_DESC)
            .context("build mic pipeline (no capture device?)")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("mic pipeline is not a Pipeline"))?;
        let sink = pipeline
            .by_name("out")
            .context("appsink 'out' missing")?
            .downcast::<AppSink>()
            .map_err(|_| anyhow!("'out' is not an AppSink"))?;

        // Honour a mute pressed before the device finished opening.
        let muted_flag = std::sync::Arc::new(AtomicBool::new(MIC_MUTED.load(Ordering::Relaxed)));
        let flag = muted_flag.clone();
        sink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    // Drop while muted *after* the encoder so unmute is instantaneous —
                    // restarting the pipeline would cost a device-open round trip.
                    if flag.load(Ordering::Relaxed) {
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    if let Some(buf) = sample.buffer() {
                        if let Ok(map) = buf.map_readable() {
                            on_frame(map.as_slice().to_vec());
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline
            .set_state(gst::State::Playing)
            .context("start mic capture (permission denied?)")?;
        Ok(Self { pipeline, muted: muted_flag })
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
}

/// Process-wide audio state, so the port-1 read loop and the UI toggles share one instance.
pub static AUDIO: Mutex<Option<Playback>> = Mutex::new(None);

/// Process-wide microphone. Opened once at first connect and kept across reconnects — a
/// device open costs a round trip (and on macOS can prompt), so it should not be repeated
/// every time the socket blips.
pub static MIC: Mutex<Option<Mic>> = Mutex::new(None);

/// Toggle the speaker; returns the new muted state.
///
/// Muting before the first frame arrives (so no `Playback` exists yet) is honoured by
/// latching the intent, which the read loop applies when it builds the pipeline.
pub fn toggle_speaker() -> bool {
    let want = !SPEAKER_MUTED.fetch_xor(true, Ordering::Relaxed);
    if let Some(p) = AUDIO.lock().unwrap().as_ref() {
        p.set_muted(want);
    }
    want
}

/// Toggle the microphone; returns the new muted state.
pub fn toggle_mic() -> bool {
    let want = !MIC_MUTED.fetch_xor(true, Ordering::Relaxed);
    if let Some(m) = MIC.lock().unwrap().as_ref() {
        m.set_muted(want);
    }
    want
}

/// Latched mute intent, so a toggle pressed before the device exists still applies.
static SPEAKER_MUTED: AtomicBool = AtomicBool::new(false);
static MIC_MUTED: AtomicBool = AtomicBool::new(false);

/// Open the microphone if it isn't already, uploading each encoded frame via `send`.
///
/// The mic is **on by default**: it opens as soon as the viewer connects, so anything
/// running in the selected clone can record the operator's room without further action.
/// `Ctrl+Alt+M` mutes it locally, and the server's `audio.micEnabled` disables it
/// fleet-wide (the server drops upstream frames when it is off).
///
/// A missing capture device is expected, not fatal — the caller keeps running with the
/// button disabled.
pub fn ensure_mic<F>(send: F)
where
    F: Fn(u64, Vec<u8>) + Send + 'static,
{
    let mut slot = MIC.lock().unwrap();
    if slot.is_some() {
        return;
    }
    let seq = AtomicU64::new(0);
    match Mic::start(move |opus| send(seq.fetch_add(1, Ordering::Relaxed), opus)) {
        Ok(m) => {
            tracing::info!("microphone open (Ctrl+Alt+M to mute)");
            *slot = Some(m);
        }
        Err(e) => tracing::warn!("microphone unavailable: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The playback sink must SYNCHRONISE, which is what engages the sink's clock slaving
    /// (`pulsesink`'s `slave-method`, "skew" by default).
    ///
    /// Regression: this shipped as `sync=false`, copied from the video sink where it is
    /// right. Audio is different — the clone captures on its sound card's crystal and these
    /// speakers run on another, differing by tens of ppm, and a non-synchronising sink does
    /// nothing to reconcile them. The difference accumulated in the queue until it saturated
    /// and leaked an *encoded* frame, desyncing `opusdec` into noise. It sounded perfect for
    /// the first minutes, which is precisely why a test is worth more than a manual check.
    #[test]
    fn playback_sink_synchronises_to_absorb_clock_drift() {
        assert!(
            PLAY_DESC.contains("sync=true"),
            "playback sink must sync=true or clock drift accumulates into decoder noise"
        );
        assert!(
            !PLAY_DESC.contains("sync=false"),
            "no element in the playback path may disable sync"
        );
    }

    /// `opusdec` is stateful, so a leak must never drop an encoded frame in steady state —
    /// the queue bound is the backstop, not the mechanism. Pinned because shrinking it would
    /// silently reintroduce the same noise under a different trigger.
    #[test]
    fn playback_queue_keeps_a_generous_bound() {
        assert!(
            PLAY_DESC.contains("max-size-time=100000000"),
            "playback queue should hold ~100ms; a tighter bound leaks encoded frames"
        );
    }

    /// Each pushed buffer is exactly one encoder frame (the port-1 framing preserved the
    /// boundary), so `opusparse` would collapse them rather than help.
    #[test]
    fn neither_pipeline_reparses_preframed_opus() {
        assert!(!PLAY_DESC.contains("opusparse"));
        assert!(!MIC_DESC.contains("opusparse"));
    }
}
