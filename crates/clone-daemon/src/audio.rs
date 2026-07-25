//! Bidirectional audio for the clone: desktop sound out, operator microphone in.
//!
//! Two GStreamer pipelines, built lazily on the server's first `AudioSubscribe` and torn
//! down on unsubscribe — only the *selected* clone is ever subscribed, so an unselected
//! clone holds no pipeline and burns no CPU.
//!
//! Unlike video (raw dmabufs shipped to the server, which owns the only VA-API GPU), audio
//! is **encoded here**: Opus costs ~1% of a core, and encoding at the endpoints keeps the
//! socket at ~12 KB/s instead of ~190 KB/s of raw PCM while letting the control-server
//! relay frames without ever decoding one.
//!
//! GStreamer rather than the raw `pipewire` crate (as [`crate::capture_pw`] uses): that
//! module exists solely because `pipewiresrc` can't surface `SPA_META_Cursor`, and there is
//! no analogous gap for audio. `pipewiresrc`/`pipewiresink` handle format negotiation,
//! resampling and reconnect for free.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use wire::socket::{AudioData, DaemonMsg};

use crate::transport::Transport;

/// 48 kHz: Opus's native rate *and* the rate of the PipeWire null sink every headless
/// clone gets, so the common path needs no resampler.
const RATE: u32 = 48_000;

/// Node name of the virtual microphone this daemon creates and feeds. Apps in the clone
/// see it as a normal capture device.
pub const MIC_NODE: &str = "rmng-mic";

/// Mono: nobody needs stereo voice, and it halves the mic bitrate.
const MIC_CHANNELS: u32 = 1;

/// Client name of our playback stream. WirePlumber will **not** auto-route a playback
/// stream into an `Audio/Source/Virtual` node (verified: the stream requests the target
/// and no link is ever formed), so [`link_mic_feed`] connects the ports by hand and needs
/// a stable name to find our own stream by.
const MIC_FEED_CLIENT: &str = "rmng-mic-feed";

/// Capture the desktop's default sink monitor, encode to Opus.
///
/// `stream.capture.sink=true` asks PipeWire for the *monitor* of a sink rather than a
/// real source. Deliberately no `target-object`: WirePlumber routes us to the current
/// default sink, so we follow `auto_null` being torn down and replaced (its
/// `fallback-sink.lua` destroys it the moment any real sink appears) without having to
/// re-resolve node ids ourselves.
///
/// The `leaky=downstream` queue sits **before** `opusenc` on purpose: dropping a raw
/// buffer under load is a click, but dropping an already-encoded frame would desync the
/// decoder's state at the far end.
const CAPTURE_DESC: &str = "\
    pipewiresrc name=src do-timestamp=true keepalive-time=1000 \
      client-name=rmng-audio-capture \
      stream-properties=\"p,stream.capture.sink=true,node.name=rmng-audio-capture\" ! \
    audioconvert ! audioresample ! \
    audio/x-raw,format=S16LE,rate=48000,channels=2 ! \
    queue max-size-buffers=8 max-size-time=200000000 leaky=downstream ! \
    opusenc bitrate=96000 frame-size=20 bitrate-type=vbr inband-fec=true ! \
    appsink name=out emit-signals=true max-buffers=8 sync=false drop=true";

/// Decode operator Opus and write it into the virtual mic node's input ports.
///
/// No `opusparse`: each appsrc buffer is exactly one encoder frame (the socket framing
/// preserved the boundary), which is what `opusdec` wants. `opusparse` is for recovering
/// boundaries from a *flat* byte stream — feeding it pre-framed buffers collapses them
/// (verified: 65 KB of concatenated frames parsed back to 0.12 s of audio).
///
/// `sync=false async=false` because we are a live source clocked upstream — the sink must
/// not block preroll waiting for a clock. `plc=true` turns a dropped relay frame into a
/// smoothed gap instead of a click.
///
/// `slave-method=resample` is the mic-direction half of the drift fix (the viewer's
/// `PLAY_DESC` carries the outbound half). The operator's microphone is clocked by their
/// sound card, this node by the clone's PipeWire graph, and the two differ by tens of ppm;
/// left unreconciled that accumulates until the queue leaks an *encoded* frame, which
/// desyncs `opusdec` into noise after minutes of use.
///
/// **`sync` stays false here, deliberately asymmetric with the viewer's sink.** Unlike
/// `pulsesink`, `pipewiresink` defaults to `slave-method=none`, so the resampling has to be
/// asked for by name — but it does NOT need `sync=true` to engage. Setting `sync=true` on
/// this sink wedges the pipeline before it reaches PLAYING (verified: `sync=true` alone
/// hangs, `slave-method=resample` alone runs clean), because a virtual source node has no
/// consumer clock to synchronise against.
fn playback_desc() -> String {
    format!(
        "appsrc name=src is-live=true format=time do-timestamp=true \
           max-buffers=16 leaky-type=downstream ! \
         opusdec plc=true ! \
         audioconvert ! audioresample ! \
         audio/x-raw,format=S16LE,rate={RATE},channels={MIC_CHANNELS} ! \
         pipewiresink name=sink client-name={MIC_FEED_CLIENT} \
           sync=false async=false slave-method=resample"
    )
}

/// Create the virtual microphone node if it isn't there yet.
///
/// `support.null-audio-sink` + `media.class=Audio/Source/Virtual` + `monitor.passthrough`
/// is the upstream virtual-mic recipe (the commented example in `pipewire.conf`). We build
/// it at runtime rather than shipping a `pipewire.conf.d` drop-in so the feature rides a
/// server update — clone binaries are injected at clone-create, but a config drop-in would
/// need a full template rebuild.
///
/// `object.linger=true` is what makes this safe: the node outlives the short-lived
/// `pw-cli` that created it (verified — without it the node vanishes when the client
/// exits). Idempotent: an existing node is left alone, so a daemon restart doesn't
/// stack duplicates.
///
/// Uses `pw-cli` rather than the `pipewire` crate's `create_object` because the latter
/// needs a dedicated blocking mainloop thread held open for the node's lifetime, which is
/// a lot of machinery for one object. `pw-cli` ships in `pipewire-bin`, a hard dependency
/// of `pipewire` itself, so it is always present in a clone.
pub fn ensure_mic_node() -> Result<()> {
    if mic_node_exists() {
        tracing::debug!("virtual mic '{MIC_NODE}' already present");
        return Ok(());
    }
    let args = format!(
        "{{ factory.name=support.null-audio-sink node.name={MIC_NODE} \
           node.description=\"RMNG Operator Microphone\" \
           media.class=Audio/Source/Virtual audio.position=[MONO] \
           audio.rate={RATE} monitor.passthrough=true object.linger=true }}"
    );
    // `pw-cli create-node` without -m returns once the object is created.
    let out = std::process::Command::new("pw-cli")
        .args(["create-node", "adapter", &args])
        .output()
        .context("spawn pw-cli create-node")?;
    if !out.status.success() {
        return Err(anyhow!(
            "pw-cli create-node failed: {}",
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        ));
    }
    tracing::info!("created virtual mic node '{MIC_NODE}'");
    Ok(())
}

fn mic_node_exists() -> bool {
    std::process::Command::new("pw-cli")
        .args(["ls", "Node"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout).contains(&format!("node.name = \"{MIC_NODE}\""))
        })
}

/// Link our playback stream's output ports to the virtual mic's input ports.
///
/// Required because WirePlumber's policy does not route a playback stream into an
/// `Audio/Source/Virtual` node — the stream sits there with its target set and no link is
/// ever created, so the mic records silence. Verified end to end: with the manual link the
/// same setup carries audio at full amplitude.
///
/// Best-effort and idempotent — `pw-link` reports an already-linked pair as an error we
/// deliberately ignore. Called after the playback pipeline reaches Playing, since the
/// stream's ports only exist once it is running.
fn link_mic_feed() {
    let out = std::process::Command::new("pw-link").args(["-o"]).output();
    let Ok(out) = out else {
        tracing::warn!("pw-link unavailable; mic will record silence");
        return;
    };
    let ports: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(&format!("{MIC_FEED_CLIENT}:")))
        .map(str::to_string)
        .collect();
    if ports.is_empty() {
        tracing::warn!("no {MIC_FEED_CLIENT} output ports yet; mic may be silent");
        return;
    }
    // The sink negotiates its own channel layout, so link whatever it exposes onto the
    // mic's single mono input rather than assuming a port name.
    for p in &ports {
        let _ = std::process::Command::new("pw-link")
            .args([p.as_str(), &format!("{MIC_NODE}:input_MONO")])
            .output();
    }
    tracing::info!("linked {} mic-feed port(s) → {MIC_NODE}", ports.len());
}

/// A running capture pipeline. Dropping it stops capture and releases the PipeWire link.
pub struct Capture {
    pipeline: gst::Pipeline,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// A running mic-playback pipeline plus the appsrc frames are pushed into.
pub struct Playback {
    pipeline: gst::Pipeline,
    src: AppSrc,
    /// Last `seq` seen, to detect relay gaps (currently logged; `opusdec plc=true`
    /// conceals them).
    last_seq: AtomicU64,
}

impl Drop for Playback {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Start capturing desktop audio. Each encoded frame is shipped as `DaemonMsg::AudioData`
/// straight from the GStreamer streaming thread, the same way cursor metadata is.
pub fn start_capture(transport: Arc<Transport>) -> Result<Capture> {
    let pipeline = gst::parse::launch(CAPTURE_DESC)
        .context("build audio capture pipeline")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("audio capture pipeline is not a Pipeline"))?;
    let sink = pipeline
        .by_name("out")
        .context("appsink 'out' missing")?
        .downcast::<AppSink>()
        .map_err(|_| anyhow!("'out' is not an AppSink"))?;

    let seq = AtomicU64::new(0);
    sink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let Some(buf) = sample.buffer() else { return Ok(gst::FlowSuccess::Ok) };
                let Ok(map) = buf.map_readable() else { return Ok(gst::FlowSuccess::Ok) };
                let msg = DaemonMsg::AudioData(AudioData {
                    seq: seq.fetch_add(1, Ordering::Relaxed),
                    opus: map.as_slice().to_vec(),
                });
                // Best-effort: a send failure means the socket is going away, and the
                // reader thread already owns that teardown. Dropping audio is free.
                let _ = transport.send(&msg, &[]);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    pipeline.set_state(gst::State::Playing).context("start audio capture")?;
    Ok(Capture { pipeline })
}

/// Start the microphone playback pipeline, feeding the virtual mic node.
pub fn start_playback() -> Result<Playback> {
    let pipeline = gst::parse::launch(&playback_desc())
        .context("build mic playback pipeline")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("mic playback pipeline is not a Pipeline"))?;
    let src = pipeline
        .by_name("src")
        .context("appsrc 'src' missing")?
        .downcast::<AppSrc>()
        .map_err(|_| anyhow!("'src' is not an AppSrc"))?;
    // Raw Opus frames, no container: `opusdec` needs the caps up front since there is no
    // header page to infer them from. Family 0 = mono/stereo.
    src.set_caps(Some(
        &gst::Caps::builder("audio/x-opus")
            .field("channel-mapping-family", 0i32)
            .field("rate", RATE as i32)
            .field("channels", MIC_CHANNELS as i32)
            .build(),
    ));

    pipeline.set_state(gst::State::Playing).context("start mic playback")?;
    // Ports exist only once the pipeline is running, and WirePlumber won't route this for
    // us — see `link_mic_feed`.
    link_mic_feed();
    Ok(Playback { pipeline, src, last_seq: AtomicU64::new(u64::MAX) })
}

impl Playback {
    /// Push one operator Opus frame at the virtual mic.
    pub fn push(&self, frame: AudioData) {
        let prev = self.last_seq.swap(frame.seq, Ordering::Relaxed);
        if prev != u64::MAX && frame.seq > prev.wrapping_add(1) {
            // `opusdec plc=true` conceals the gap; log it so a lossy link is diagnosable.
            tracing::debug!("mic gap: {} frame(s) lost", frame.seq - prev - 1);
        }
        let buf = gst::Buffer::from_mut_slice(frame.opus);
        // A push failure means the pipeline is shutting down — the next unsubscribe drops it.
        if let Err(e) = self.src.push_buffer(buf) {
            tracing::debug!("mic push failed: {e:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mic-playback sink must resample to absorb clock drift, and must NOT synchronise.
    ///
    /// That combination looks inconsistent with the viewer's playback sink, which does the
    /// opposite (`sync=true`, relying on `pulsesink`'s default slaving). Both are needed and
    /// neither generalises:
    ///
    ///   - `pipewiresink` defaults to `slave-method=none`, unlike `pulsesink`'s "skew", so
    ///     the resampling has to be requested by name — without it the operator's sound-card
    ///     clock and the clone's PipeWire graph diverge until the queue leaks an encoded
    ///     frame and `opusdec` desyncs into noise.
    ///   - `sync=true` on THIS sink wedges the pipeline before it reaches PLAYING, because a
    ///     virtual source node offers no consumer clock to synchronise against. Verified
    ///     directly: `sync=true` alone hangs, `slave-method=resample` alone runs clean.
    ///
    /// So this pins both halves — a future "make these two sinks consistent" edit would
    /// reintroduce either the noise or a hang.
    #[test]
    fn mic_sink_resamples_without_synchronising() {
        let desc = playback_desc();
        assert!(
            desc.contains("slave-method=resample"),
            "pipewiresink defaults to slave-method=none; drift needs it named explicitly"
        );
        assert!(
            desc.contains("sync=false"),
            "sync=true wedges this sink before PLAYING — a virtual source has no consumer clock"
        );
    }

    /// The leaky queue sits BEFORE the encoder on the capture path. Dropping a raw buffer
    /// under load is a click; dropping an encoded frame desyncs the far-end decoder.
    #[test]
    fn capture_leaks_raw_buffers_not_encoded_frames() {
        let q = CAPTURE_DESC.find("leaky=downstream").expect("capture queue is leaky");
        let enc = CAPTURE_DESC.find("opusenc").expect("capture encodes");
        assert!(q < enc, "the leaky queue must precede opusenc, not follow it");
    }
}
