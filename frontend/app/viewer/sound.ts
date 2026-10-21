// The selected clone's sound in the viewer window: the AudioContext and its worklet
// (`./sound-worklet.ts`). The worker decodes the packets and sends the samples to the worklet
// on a port of their own; this page thread only starts the sound.
//
// Chrome starts an AudioContext only after a click or a key in the page. A viewer window opens
// with none, so the sound starts at the first click or key. The window asks the server for the
// sound only while the context runs, so a window nobody clicked yet gets no packets.
import workletUrl from "./sound-worklet.ts?worker&url";

import type { SoundStats, WorkerIn } from "./protocol";

/** Events that let Chrome start the AudioContext. Capture phase, so the input handlers that
 *  stop key events do not hide them. */
const GESTURES = ["pointerdown", "keydown"] as const;

export function attachSound(post: (m: WorkerIn, transfer?: Transferable[]) => void): {
  detach(): void;
} {
  const ctx = new AudioContext({ sampleRate: 48000, latencyHint: "interactive" });
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
    if (ctx.state === "suspended") ctx.resume().then(update, update);
  };
  ctx.onstatechange = update;
  for (const g of GESTURES) window.addEventListener(g, resume, true);

  (async () => {
    await ctx.audioWorklet.addModule(workletUrl);
    if (detached) return;
    const node = new AudioWorkletNode(ctx, "rmng-sound", {
      numberOfInputs: 0,
      outputChannelCount: [2],
    });
    node.connect(ctx.destination);
    // For a look from the console or a test: `rmngSound` is the buffer's last report.
    node.port.onmessage = (e: MessageEvent<SoundStats>) => {
      (globalThis as { rmngSound?: SoundStats }).rmngSound = e.data;
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
      for (const g of GESTURES) window.removeEventListener(g, resume, true);
      ctx.close();
    },
  };
}
