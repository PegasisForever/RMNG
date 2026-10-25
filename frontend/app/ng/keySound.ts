// The click a key makes when a finger comes down on it, close to the iPhone's own keyboard:
// a short bright tick, with a different one for Delete and a lower one for the wide keys
// (Space, Return, Shift and the other modifiers), as iOS has.
//
// The sounds are made here, not loaded: each is a burst of band-passed noise (the tick) over a
// damped tone (the body), a few milliseconds long, rendered once into an AudioBuffer so a key
// plays with no delay. iOS only starts audio inside a tap, so the first touches unlock it
// (`unlock`). The audio session is "ambient", as the iOS keyboard's is: the ring/silent
// switch mutes it, and it does not stop music that plays.
import type { Key } from "./keyboard";

export type ClickKind = "key" | "delete" | "modifier";

/** What each kind is made of: the tick's band centre (Hz), the body's pitch (Hz), how fast
 *  each dies away (ms), how loud the body is against the tick, and the level. */
const VOICE: Record<ClickKind, { band: number; body: number; tick: number; ring: number; mix: number; level: number }> = {
  key: { band: 4200, body: 1250, tick: 2.2, ring: 5, mix: 0.35, level: 0.55 },
  delete: { band: 3300, body: 950, tick: 2.6, ring: 6, mix: 0.45, level: 0.6 },
  modifier: { band: 2400, body: 640, tick: 3.2, ring: 8, mix: 0.55, level: 0.6 },
};

/** The sound's length; everything is silent well before it ends. */
const LENGTH_MS = 45;

/** Backspace, and Delete under Fn (evdev). */
const DELETE_CODES = new Set([14, 111]);

/** Which click `key` makes: Delete has its own, the keys that are not characters (and Space) a
 *  lower one, and every other key the plain tick. */
export function clickKind(key: Key): ClickKind {
  if (DELETE_CODES.has(key.code)) return "delete";
  return key.mod || key.fn || key.icon || key.units > 1 || key.label.length > 1 ? "modifier" : "key";
}

/** One click's samples at `rate` Hz, its peak at `VOICE.level`. Pure (seeded noise), so a test
 *  can look at it. */
export function renderClick(kind: ClickKind, rate: number): Float32Array<ArrayBuffer> {
  const v = VOICE[kind];
  const n = Math.round((rate * LENGTH_MS) / 1000);
  const out = new Float32Array(n);
  // A band-pass biquad (RBJ, constant 0 dB peak gain) over the noise
  const w = (2 * Math.PI * v.band) / rate;
  const alpha = Math.sin(w) / (2 * 1.4);
  const a0 = 1 + alpha;
  const b0 = alpha / a0;
  const b2 = -alpha / a0;
  const a1 = (-2 * Math.cos(w)) / a0;
  const a2 = (1 - alpha) / a0;
  let x1 = 0;
  let x2 = 0;
  let y1 = 0;
  let y2 = 0;
  let seed = 0x2545f491;
  const noise = () => {
    seed ^= seed << 13;
    seed ^= seed >>> 17;
    seed ^= seed << 5;
    return ((seed >>> 0) / 0xffffffff) * 2 - 1;
  };
  const attack = Math.round(rate * 0.0004);
  for (let i = 0; i < n; i++) {
    const t = i / rate;
    const x = noise() * Math.exp(-t / (v.tick / 1000));
    const y = b0 * x + b2 * x2 - a1 * y1 - a2 * y2;
    x2 = x1;
    x1 = x;
    y2 = y1;
    y1 = y;
    const body = Math.sin(2 * Math.PI * v.body * t) * Math.exp(-t / (v.ring / 1000)) * v.mix;
    // A fraction of a millisecond of fade-in, so the first sample is no pop
    out[i] = (y + body) * Math.min(1, i / Math.max(1, attack));
  }
  let peak = 0;
  for (const s of out) peak = Math.max(peak, Math.abs(s));
  if (peak > 0) for (let i = 0; i < n; i++) out[i] *= v.level / peak;
  return out;
}

/** The phone's audio session (Safari 16.4+), not in TypeScript's DOM types yet. */
type WithAudioSession = Navigator & { audioSession?: { type: string } };

export class KeySounds {
  private ctx: AudioContext | null = null;
  private buffers = new Map<ClickKind, AudioBuffer>();

  /** Start the audio: call it from inside a touch (iOS starts audio only in a user gesture).
   *  Safe to call on every touch. */
  unlock() {
    if (typeof window === "undefined" || typeof AudioContext === "undefined") return;
    if (!this.ctx) {
      const session = (navigator as WithAudioSession).audioSession;
      if (session) session.type = "ambient";
      this.ctx = new AudioContext({ latencyHint: "interactive" });
      for (const kind of Object.keys(VOICE) as ClickKind[]) {
        const samples = renderClick(kind, this.ctx.sampleRate);
        const buffer = this.ctx.createBuffer(1, samples.length, this.ctx.sampleRate);
        buffer.copyToChannel(samples, 0);
        this.buffers.set(kind, buffer);
      }
    }
    // Suspended until a gesture, and "interrupted" again after the app was in the background
    if (this.ctx.state !== "running") {
      void this.ctx.resume().catch(() => {});
      // A sound played inside the gesture is what lets older iOS start the output
      const silent = this.ctx.createBufferSource();
      silent.buffer = this.ctx.createBuffer(1, 1, this.ctx.sampleRate);
      silent.connect(this.ctx.destination);
      silent.start();
    }
  }

  /** Play the click for `key` now. Nothing until the audio is unlocked. */
  play(key: Key) {
    const ctx = this.ctx;
    const buffer = this.buffers.get(clickKind(key));
    if (!ctx || !buffer || ctx.state !== "running") return;
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(ctx.destination);
    src.start();
  }
}
