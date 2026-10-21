//! The clone's desktop sound, out to the viewer: capture the default sink's monitor, encode
//! Opus, ship each packet as [`DaemonMsg::Audio`].
//!
//! It runs while the server keeps this clone's capture on (`ServerMsg::Capture`), so only the
//! watched clone captures sound, and stops with it. A screenshot's short capture wake does not
//! start it.
//!
//! Encoded here, not on the server: Opus costs about 1 % of a core, keeps the clone socket at
//! about 12 KB/s instead of 190 KB/s of raw samples, and the server relays packets without a
//! single audio element of its own.
//!
//! GStreamer `pipewiresrc`, not the raw `pipewire` crate that video uses: video needs the raw
//! stream only for the cursor metadata, and sound has no such gap. `pipewiresrc` does the
//! format negotiation and follows the default sink when it changes.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use wire::socket::{AUDIO_FRAME_MS, AudioPacket, DaemonMsg};

use crate::transport::Transport;

/// The capture pipeline.
///
/// - `stream.capture.sink=true` asks PipeWire for the monitor of a sink, not a source. No
///   `target-object`: WirePlumber routes the stream to the default sink (the "Dummy Output" a
///   clone with no sound card has), and moves it when the default changes.
/// - `node.latency=512/48000` (10.7 ms, about one packet): PipeWire runs its graph in blocks
///   of the smallest latency a stream asks for, rounded down to a power of two. Without it the
///   graph took the 2048 frames (43 ms) that `pw-play` asks for, the packets left in bursts of
///   four, and the browser's buffer ran dry between the bursts (measured: 23 late packets and
///   21 dropped in 30 s of beeps).
/// - The leaky queue is before `opusenc`: a raw buffer dropped under load is a click, but an
///   encoded packet dropped would put the browser's decoder out of step.
/// - `opusenc audio-type=generic`: desktop sound is music and video as often as voice. VBR at
///   96 kbit/s is near transparent for stereo music; silence costs a few bytes per packet.
fn capture_desc() -> String {
    format!(
        "pipewiresrc name=src do-timestamp=true keepalive-time=1000 \
           client-name=rmng-audio-capture \
           stream-properties=\"p,stream.capture.sink=true,node.name=rmng-audio-capture,\
             node.latency=512/48000\" ! \
         audioconvert ! audioresample ! \
         audio/x-raw,format=S16LE,rate=48000,channels=2 ! \
         queue max-size-buffers=0 max-size-bytes=0 max-size-time=200000000 leaky=downstream ! \
         opusenc bitrate=96000 frame-size={AUDIO_FRAME_MS} bitrate-type=vbr audio-type=generic ! \
         appsink name=out emit-signals=false max-buffers=32 drop=true sync=false"
    )
}

/// A running capture. Dropping it stops the pipeline and disconnects from the sink.
pub struct Capture {
    pipeline: gst::Pipeline,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Start capturing the desktop sound. Each packet goes to the server straight from the
/// GStreamer streaming thread, as cursor messages go from the capture thread.
pub fn start(transport: Arc<Transport>) -> Result<Capture> {
    let pipeline = gst::parse::launch(&capture_desc())
        .context("build the audio pipeline")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("the audio pipeline is not a Pipeline"))?;
    let sink = pipeline
        .by_name("out")
        .context("appsink 'out' is missing")?
        .downcast::<AppSink>()
        .map_err(|_| anyhow!("'out' is not an AppSink"))?;
    let mut seq = 0u32;
    let mut gate = SilenceGate::new();
    sink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                // Every packet takes a number, also one the silence rule holds back, so the
                // numbers count time.
                let n = seq;
                seq = seq.wrapping_add(1);
                let opus = match gate.next(map.as_slice()) {
                    Pass::Send => map.as_slice().to_vec(),
                    Pass::Quiet => Vec::new(),
                    Pass::Skip => return Ok(gst::FlowSuccess::Ok),
                };
                let msg = DaemonMsg::Audio(AudioPacket { seq: n, opus });
                // A failed send means the socket is going away; the reader thread owns that.
                let _ = transport.send(&msg, &[]);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline
        .set_state(gst::State::Playing)
        .context("start the audio pipeline")?;
    Ok(Capture { pipeline })
}

/// libopus encodes digital silence as a packet of at most this many bytes (measured: 3 bytes
/// for each 10 ms packet of the Dummy Output's monitor with nothing playing). Any sound makes
/// a larger one.
const SILENT_PACKET_BYTES: usize = 3;
/// After this many silent packets in a row (100 ms), the clone stops sending.
const SILENT_BEFORE_QUIET: u32 = 10;

/// What to do with one encoded packet.
#[derive(Debug, PartialEq, Eq)]
enum Pass {
    Send,
    /// Send an empty packet: the quiet marker. The browser then knows that the packets that do
    /// not come are silence, not late.
    Quiet,
    Skip,
}

/// Sends nothing while the desktop is silent, as Selkies does. A clone is silent nearly all
/// the time, and else it sends 100 packets a second that the browser decodes to zeros.
struct SilenceGate {
    silent: u32,
}

impl SilenceGate {
    /// A run that starts silent sends the quiet marker with its first packet: the first
    /// packets of a run come unevenly, and the browser would take that as late packets.
    fn new() -> SilenceGate {
        SilenceGate { silent: SILENT_BEFORE_QUIET - 1 }
    }

    fn next(&mut self, packet: &[u8]) -> Pass {
        if packet.len() > SILENT_PACKET_BYTES {
            self.silent = 0;
            return Pass::Send;
        }
        self.silent = self.silent.saturating_add(1);
        match self.silent {
            n if n < SILENT_BEFORE_QUIET => Pass::Send,
            n if n == SILENT_BEFORE_QUIET => Pass::Quiet,
            _ => Pass::Skip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The leaky queue is before the encoder: a dropped raw buffer is a click, a dropped
    /// encoded packet puts the far decoder out of step.
    #[test]
    fn capture_drops_raw_buffers_not_packets() {
        let desc = capture_desc();
        let q = desc.find("leaky=downstream").expect("the queue is leaky");
        assert!(q < desc.find("opusenc").expect("it encodes"));
    }

    /// A short silence inside sound goes as it is; a long one sends one quiet marker and then
    /// nothing until the sound comes back.
    #[test]
    fn silence_sends_one_quiet_marker_then_nothing() {
        let mut g = SilenceGate::new();
        assert_eq!(g.next(&[0; 40]), Pass::Send);
        for _ in 1..SILENT_BEFORE_QUIET {
            assert_eq!(g.next(&[0; 3]), Pass::Send);
        }
        assert_eq!(g.next(&[0; 3]), Pass::Quiet);
        assert_eq!(g.next(&[0; 3]), Pass::Skip);
        assert_eq!(g.next(&[0; 3]), Pass::Skip);
        assert_eq!(g.next(&[0; 40]), Pass::Send);
        assert_eq!(g.next(&[0; 3]), Pass::Send);
        // A run that starts silent is quiet at once.
        let mut g = SilenceGate::new();
        assert_eq!(g.next(&[0; 3]), Pass::Quiet);
        assert_eq!(g.next(&[0; 3]), Pass::Skip);
    }

    /// The pipeline parses with the elements a clone has (pipewiresrc, opusenc).
    #[test]
    fn capture_pipeline_builds() {
        if gst::init().is_err()
            || gst::ElementFactory::find("pipewiresrc").is_none()
            || gst::ElementFactory::find("opusenc").is_none()
        {
            return; // a build box without the plugins
        }
        gst::parse::launch(&capture_desc()).expect("the pipeline parses");
    }
}
