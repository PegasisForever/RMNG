// The AudioWorklet processor that plays the selected clone's sound. The worker decodes the
// packets and sends the samples here on a MessagePort of their own, so no sound passes through
// the page thread; the page only owns the AudioContext. The buffer rules are in
// `./sound-buffer.ts`.
//
// Relative imports only: Vite builds the worklet (`?worker&url`) without the app's path aliases.
import type { SoundIn } from "./protocol";
import { type BufferLimits, SoundBuffer } from "./sound-buffer";

// The AudioWorklet scope; TypeScript's DOM library does not declare it.
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
}
declare function registerProcessor(
  name: string,
  ctor: new (options?: { processorOptions?: { limits?: BufferLimits } }) => AudioWorkletProcessor,
): void;

/** How often the processor tells the page how the buffer does (`SoundStats`). */
const STATS_MS = 1000;

class SoundProcessor extends AudioWorkletProcessor {
  private buf: SoundBuffer;
  private statsAt = 0;

  constructor(options?: { processorOptions?: { limits?: BufferLimits } }) {
    super();
    this.buf = new SoundBuffer(options?.processorOptions?.limits);
    // The page sends the worker's port once; the samples come on it.
    this.port.onmessage = (e: MessageEvent<MessagePort>) => {
      e.data.onmessage = (m: MessageEvent<SoundIn>) => this.onSound(m.data);
    };
  }

  private onSound(m: SoundIn) {
    if (m.t === "pcm") this.buf.push(m);
    else if (m.t === "quiet") this.buf.setQuiet();
    else this.buf.reset();
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const [l, r] = outputs[0];
    this.buf.render(l, r ?? new Float32Array(l.length));
    const now = Date.now();
    if (now - this.statsAt >= STATS_MS) {
      this.statsAt = now;
      const { target, late, dropped } = this.buf;
      this.port.postMessage({ target, late, dropped });
    }
    return true;
  }
}

registerProcessor("rmng-sound", SoundProcessor);
