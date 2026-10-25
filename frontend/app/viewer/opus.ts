// The Opus decoder for the clone's sound, in the viewer worker. WebCodecs' AudioDecoder where
// the browser has it and it takes Opus; libopus in WebAssembly (`opus-decoder`) where not, as
// selkies does: Safari before 26 has no AudioDecoder, and a browser can have one that does not
// take Opus. The WebAssembly decoder loads only when it is needed.
import type { OpusDecoder } from "opus-decoder";

export const OPUS_CONFIG: AudioDecoderConfig = { codec: "opus", sampleRate: 48000, numberOfChannels: 2 };

/** Whether this browser's WebCodecs decodes Opus. */
const webcodecs: Promise<boolean> =
  typeof AudioDecoder === "undefined"
    ? Promise.resolve(false)
    : AudioDecoder.isConfigSupported(OPUS_CONFIG).then(
        (s) => s.supported === true,
        () => false,
      );

export interface Opus {
  /** Resolves when the decoder takes packets. */
  ready: Promise<void>;
  /** Decode one packet (`seq` orders them); its samples come out through `onPcm`. */
  decode(seq: number, data: Uint8Array): void;
  close(): void;
}

/** A decoder whose samples (left, right) come out through `onPcm`. Packets given to it before
 *  it is ready (the WebAssembly one loads first) are dropped. */
export function openOpus(
  onPcm: (l: Float32Array, r: Float32Array) => void,
  onError: (e: unknown) => void,
  frameMs: number,
): Opus {
  let closed = false;
  let impl: Opus | null = null;
  const ready = webcodecs.then(async (yes) => {
    if (closed) return;
    impl = yes ? webcodecsOpus(onPcm, onError, frameMs) : await wasmOpus(onPcm, onError);
    if (closed) impl.close();
  });
  ready.catch(onError);
  return {
    ready,
    decode: (seq, data) => impl?.decode(seq, data),
    close() {
      closed = true;
      impl?.close();
    },
  };
}

function webcodecsOpus(
  onPcm: (l: Float32Array, r: Float32Array) => void,
  onError: (e: unknown) => void,
  frameMs: number,
): Opus {
  const dec = new AudioDecoder({
    output(d) {
      const l = new Float32Array(d.numberOfFrames);
      const r = new Float32Array(d.numberOfFrames);
      d.copyTo(l, { planeIndex: 0, format: "f32-planar" });
      d.copyTo(r, { planeIndex: d.numberOfChannels > 1 ? 1 : 0, format: "f32-planar" });
      d.close();
      onPcm(l, r);
    },
    error: onError,
  });
  dec.configure(OPUS_CONFIG);
  return {
    ready: Promise.resolve(),
    decode(seq, data) {
      if (dec.state !== "configured") return;
      dec.decode(new EncodedAudioChunk({ type: "key", timestamp: seq * frameMs * 1000, data }));
    },
    close() {
      if (dec.state !== "closed") dec.close();
    },
  };
}

async function wasmOpus(
  onPcm: (l: Float32Array, r: Float32Array) => void,
  onError: (e: unknown) => void,
): Promise<Opus> {
  const { OpusDecoder } = await import("opus-decoder");
  const dec: OpusDecoder = new OpusDecoder({ channels: 2 });
  await dec.ready;
  let freed = false;
  return {
    ready: Promise.resolve(),
    decode(_seq, data) {
      if (freed) return;
      try {
        const { channelData, samplesDecoded } = dec.decodeFrame(data);
        if (!samplesDecoded) return;
        // Copies: the worker hands them on to the worklet, whole
        const l = channelData[0].slice(0, samplesDecoded);
        const r = (channelData[1] ?? channelData[0]).slice(0, samplesDecoded);
        onPcm(l, r);
      } catch (e) {
        onError(e);
      }
    },
    close() {
      freed = true;
      dec.free();
    },
  };
}
