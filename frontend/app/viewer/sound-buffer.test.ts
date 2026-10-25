import { expect, test } from "bun:test";

import {
  DECAY_BLOCKS,
  FADE_FRAMES,
  MAX_PACKETS,
  PACKET_FRAMES,
  SoundBuffer,
  TARGET_MAX,
  TARGET_MIN,
  TRIM_BLOCKS,
} from "./sound-buffer";

const BLOCK = 128;

/** A packet of one value on both channels. */
const packet = (v: number) => ({
  l: new Float32Array(PACKET_FRAMES).fill(v),
  r: new Float32Array(PACKET_FRAMES).fill(-v),
});

/** A packet of a ramp: frame i of packet n is n * PACKET_FRAMES + i, scaled down. */
const ramp = (n: number) => {
  const l = new Float32Array(PACKET_FRAMES).map((_, i) => (n * PACKET_FRAMES + i) / 1e6);
  return { l, r: l.slice() };
};

/** Render `blocks` blocks; the left channel of all of them. */
function play(b: SoundBuffer, blocks: number): number[] {
  const out: number[] = [];
  for (let i = 0; i < blocks; i++) {
    const l = new Float32Array(BLOCK);
    const r = new Float32Array(BLOCK);
    b.render(l, r);
    out.push(...l);
  }
  return out;
}

test("the sound starts only once `target` packets wait, and fades in", () => {
  const b = new SoundBuffer();
  b.push(packet(0.5));
  expect(play(b, 1).every((v) => v === 0)).toBe(true);
  b.push(packet(0.5));
  const out = play(b, 2);
  expect(out[0]).toBeGreaterThan(0);
  expect(out[0]).toBeLessThan(0.1);
  expect(out[FADE_FRAMES]).toBeCloseTo(0.5);
  expect(out[BLOCK + 5]).toBeCloseTo(0.5);
});

test("frames play in order, with nothing lost or repeated", () => {
  const b = new SoundBuffer();
  for (let n = 0; n < 4; n++) b.push(ramp(n));
  const out = play(b, 12).slice(FADE_FRAMES, 4 * PACKET_FRAMES - FADE_FRAMES);
  out.forEach((v, i) => expect(v).toBeCloseTo((i + FADE_FRAMES) / 1e6, 9));
});

test("a late packet ramps the sound down to 0 and makes the target larger", () => {
  const b = new SoundBuffer();
  b.push(packet(0.5));
  b.push(packet(0.5));
  // 960 frames wait: 7 full blocks, then the 9th runs out.
  const out = play(b, 9);
  const end = 2 * PACKET_FRAMES;
  expect(out[end - 1]).toBeCloseTo(0.5);
  expect(out[end]).toBeCloseTo(0.5);
  expect(out[end + FADE_FRAMES / 2]).toBeCloseTo(0.25, 1);
  expect(out.slice(end + FADE_FRAMES).every((v) => v === 0)).toBe(true);
  // No step anywhere in the ramp.
  for (let i = end; i < end + FADE_FRAMES; i++) {
    expect(Math.abs(out[i + 1] - out[i])).toBeLessThan(0.01);
  }
  expect(b.target).toBe(TARGET_MIN + 1);
  expect(b.late).toBe(1);
  // It now waits for three packets.
  b.push(packet(0.5));
  b.push(packet(0.5));
  expect(play(b, 1).every((v) => v === 0)).toBe(true);
});

test("the end of a sound after the quiet marker does not make the target larger", () => {
  const b = new SoundBuffer();
  b.push(packet(0.5));
  b.push(packet(0.5));
  b.setQuiet();
  play(b, 10);
  expect(b.target).toBe(TARGET_MIN);
  expect(b.late).toBe(0);
});

test("the quiet marker plays out the last packets without waiting for `target`", () => {
  const b = new SoundBuffer();
  b.push(packet(0.5));
  b.setQuiet();
  expect(play(b, 1)[FADE_FRAMES]).toBeCloseTo(0.5);
});

test("the target never grows past TARGET_MAX", () => {
  const b = new SoundBuffer();
  for (let i = 0; i < 10; i++) {
    for (let n = 0; n < b.target; n++) b.push(packet(0.5));
    play(b, 40);
  }
  expect(b.target).toBe(TARGET_MAX);
});

test("too many packets drop one, with no step in a steady sound", () => {
  const b = new SoundBuffer();
  b.push(packet(0.5));
  b.push(packet(0.5));
  play(b, 2);
  for (let n = 0; n < MAX_PACKETS; n++) b.push(packet(0.5));
  expect(b.dropped).toBe(1);
  const out = play(b, 10);
  for (const v of out) expect(v).toBeCloseTo(0.5);
});

test("a drop before the sound starts skips the first packet", () => {
  const b = new SoundBuffer();
  for (let n = 0; n <= MAX_PACKETS; n++) b.push(ramp(n));
  expect(b.dropped).toBe(1);
  const out = play(b, 1);
  expect(out[FADE_FRAMES]).toBeCloseTo((PACKET_FRAMES + FADE_FRAMES) / 1e6, 9);
});

test("a drop while the sound plays crossfades from before the packet to after it", () => {
  const b = new SoundBuffer();
  b.push(ramp(0));
  b.push(ramp(1));
  play(b, 1);
  for (let n = 2; n <= MAX_PACKETS; n++) b.push(ramp(n));
  expect(b.dropped).toBe(1);
  const out = play(b, 2);
  // Between the two sides through the crossfade, then on from after the dropped length.
  for (let i = 0; i < FADE_FRAMES; i++) {
    expect(out[i]).toBeGreaterThanOrEqual((BLOCK + i) / 1e6 - 1e-9);
    expect(out[i]).toBeLessThanOrEqual((BLOCK + PACKET_FRAMES + i) / 1e6 + 1e-9);
  }
  expect(out[FADE_FRAMES - 1]).toBeCloseTo((BLOCK + PACKET_FRAMES + FADE_FRAMES - 1) / 1e6, 9);
  for (let i = FADE_FRAMES; i < 2 * BLOCK; i++) {
    expect(out[i]).toBeCloseTo((BLOCK + PACKET_FRAMES + i) / 1e6, 9);
  }
});

test("a queue that stays long is trimmed", () => {
  const b = new SoundBuffer();
  for (let n = 0; n < 5; n++) b.push(packet(0.5));
  // Keep 5 packets waiting: one packet in for each packet played.
  let pushed = 0;
  for (let i = 1; i <= TRIM_BLOCKS + 10; i++) {
    play(b, 1);
    while (pushed < (i * BLOCK) / PACKET_FRAMES) {
      b.push(packet(0.5));
      pushed++;
    }
  }
  expect(b.dropped).toBe(1);
});

test("a clean period with slack lets the target fall", () => {
  const b = new SoundBuffer();
  b.target = 4;
  for (let n = 0; n < 5; n++) b.push(packet(0.5));
  let pushed = 0;
  for (let i = 1; i <= DECAY_BLOCKS + 1; i++) {
    play(b, 1);
    while (pushed < (i * BLOCK) / PACKET_FRAMES) {
      b.push(packet(0.5));
      pushed++;
    }
  }
  expect(b.target).toBe(3);
  expect(b.late).toBe(0);
});

test("a queue that nearly runs dry, then gets its packet in time, does not dip", () => {
  const b = new SoundBuffer();
  b.push(packet(0.5));
  b.push(packet(0.5));
  // Each packet comes just before the queue would run dry.
  const out: number[] = [];
  let frames = 2 * PACKET_FRAMES;
  for (let i = 0; i < 200; i++) {
    if (frames < BLOCK) {
      b.push(packet(0.5));
      frames += PACKET_FRAMES;
    }
    out.push(...play(b, 1));
    frames -= BLOCK;
  }
  for (const v of out.slice(FADE_FRAMES)) expect(v).toBeCloseTo(0.5);
  expect(b.late).toBe(0);
});

test("a reset drops what waits", () => {
  const b = new SoundBuffer();
  b.push(packet(0.5));
  b.push(packet(0.5));
  b.reset();
  expect(play(b, 4).every((v) => v === 0)).toBe(true);
});

test("wider limits (the phone's): it starts at their minimum, grows to their maximum, and drops past it", () => {
  const b = new SoundBuffer({ min: 4, max: 15 });
  expect(b.target).toBe(4);
  for (let i = 0; i < 3; i++) b.push(packet(0.5));
  expect(play(b, 1).every((v) => v === 0)).toBe(true); // 3 of 4 wait: not yet
  // Late underruns, one after another, deepen it up to the maximum and no further
  for (let i = 0; i < 20; i++) {
    for (let k = 0; k < b.target; k++) b.push(packet(0.5));
    play(b, 100); // until it runs dry: a late underrun each time

  }
  expect(b.target).toBe(15);
  // The ceiling is as far above the maximum as the default one is above its own
  const d = new SoundBuffer({ min: 4, max: 15 });
  for (let i = 0; i < 15 + MAX_PACKETS - TARGET_MAX + 1; i++) d.push(packet(0.5));
  expect(d.dropped).toBe(1);
});
