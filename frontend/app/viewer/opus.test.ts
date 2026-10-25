import { expect, test } from "bun:test";

import { SINE_440_PACKETS } from "./__fixtures__/sineOpus";
import { openOpus } from "./opus";
import { AUDIO_FRAME_MS } from "./protocol";

// Bun has no WebCodecs, so this is the libopus-in-WebAssembly path a browser without
// AudioDecoder (Safari before 26) takes
test("with no AudioDecoder, the packets decode on libopus in WebAssembly: 10 ms of the tone each", async () => {
  expect(typeof AudioDecoder).toBe("undefined");
  const out: [Float32Array, Float32Array][] = [];
  const errors: unknown[] = [];
  const opus = openOpus((l, r) => out.push([l, r]), (e) => errors.push(e), AUDIO_FRAME_MS);
  await opus.ready;
  SINE_440_PACKETS.forEach((b64, seq) => opus.decode(seq, Uint8Array.from(Buffer.from(b64, "base64"))));
  opus.close();
  expect(errors).toEqual([]);
  expect(out.length).toBe(SINE_440_PACKETS.length);
  for (const [l, r] of out) {
    expect(l.length).toBe(480);
    expect(r.length).toBe(480);
  }
  // After the decoder settles, a tone at ffmpeg's sine level (1/8): RMS about 0.088
  const tail = out.slice(5).flatMap(([l]) => [...l]);
  const rms = Math.sqrt(tail.reduce((a, x) => a + x * x, 0) / tail.length);
  expect(rms).toBeGreaterThan(0.05);
  expect(rms).toBeLessThan(0.15);
});
