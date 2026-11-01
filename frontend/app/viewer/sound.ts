// The selected clone's sound in the page: the AudioContext and its worklet
// (`./sound-worklet.ts`). The worker decodes the packets and sends the samples to the worklet
// on a port of their own; this page thread only starts the sound.
//
// Chrome starts an AudioContext only after a click or a key in the page. A monitor window opens
// with none, so the sound starts at the first click or key. The window asks the server for the
// sound only while the context runs, so a window nobody clicked yet gets no packets.
//
// iOS (the phone) is stricter, as selkies found: there a touch counts only as it ends,
// so the lift and the click resume the context too, and iOS stops a running context
// ("interrupted") when the app goes to the background, so every gesture tries again. The
// page's audio session is "playback", as a video app's: the ring/silent switch does not mute
// the clone.
import workletUrl from "./sound-worklet.ts?worker&url";

import type { SoundReport, SoundStats, WorkerIn } from "./protocol";
import type { BufferLimits } from "./sound-buffer";

/** Events that let Chrome start the AudioContext. Capture phase, so the input handlers that
 *  stop key events do not hide them. */
const GESTURES = ["pointerdown", "pointerup", "touchend", "click", "keydown"] as const;

/** The phone's audio session (Safari 16.4+), not in TypeScript's DOM types yet. */
type WithAudioSession = Navigator & { audioSession?: { type: string } };

/** The jitter buffer's range (`./sound-buffer.ts`), in 10 ms packets: 40 to 150 ms. Over a
 *  phone's Wi-Fi packets come less evenly than to a desktop, and the phone also decodes the
 *  video, so the buffer is deep. */
const LIMITS: BufferLimits = { min: 4, max: 15 };

/** Play the clone's sound; `onReport` gets the worklet's report, once a second. */
export function attachSound(
  post: (m: WorkerIn, transfer?: Transferable[]) => void,
  onReport: (r: SoundReport) => void,
): {
  detach(): void;
} {
  const session = (navigator as WithAudioSession).audioSession;
  try {
    if (session && session.type !== "play-and-record") session.type = "playback";
  } catch {
    // the browser refused the type
  }
  // "playback" gives the phone's audio thread a longer output buffer than "interactive", so it
  // misses fewer deadlines while the same phone decodes the video
  const ctx = new AudioContext({ sampleRate: 48000, latencyHint: "playback" });
  /** The worker has its port to the worklet. */
  let ready = false;
  /** What the worker was last told: whether packets flow. */
  let flowing = false;
  let detached = false;

  const update = () => {
    const flow = !detached && ready && ctx.state === "running";
    if (flow === flowing) return;
    flowing = flow;
    post({ type: "sound", on: flow });
  };
  const resume = () => {
    if (ctx.state !== "running" && ctx.state !== "closed") ctx.resume().then(update, update);
  };
  ctx.onstatechange = update;
  for (const g of GESTURES) window.addEventListener(g, resume, { capture: true, passive: true });

  (async () => {
    await ctx.audioWorklet.addModule(workletUrl);
    if (detached) return;
    const node = new AudioWorkletNode(ctx, "rmng-sound", {
      numberOfInputs: 0,
      outputChannelCount: [2],
      processorOptions: { limits: LIMITS },
    });
    node.connect(ctx.destination);
    // For a look from the console or a test: `rmngSound` is the buffer's last report.
    node.port.onmessage = (e: MessageEvent<SoundStats>) => {
      (globalThis as { rmngSound?: SoundStats }).rmngSound = e.data;
      onReport({
        ...e.data,
        sampleRate: ctx.sampleRate,
        baseLatency: ctx.baseLatency ?? 0,
        outputLatency: ctx.outputLatency ?? 0,
      });
    };
    const channel = new MessageChannel();
    node.port.postMessage(channel.port2, [channel.port2]);
    post({ type: "sound", on: false, port: channel.port1 }, [channel.port1]);
    ready = true;
    update();
  })().catch((e) => console.warn("sound did not start:", e));

  return {
    detach() {
      detached = true;
      for (const g of GESTURES) window.removeEventListener(g, resume, { capture: true });
      ctx.close();
    },
  };
}
