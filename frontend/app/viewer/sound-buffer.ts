// The jitter buffer between the sound packets from the socket and the speakers. It runs in the
// AudioWorklet (`./sound-worklet.ts`), which calls `render` for each block of 128 frames; it is
// a plain class so that a test can drive it.
//
// The rules are those of Selkies' worklet, written again here:
// - Play only once `target` packets wait, so a packet that comes a little late still finds sound
//   in front of it. `target` starts at TARGET_MIN.
// - When the packets run out (an underrun), fade out and wait for `target` packets again. An
//   underrun after the quiet marker is the end of a sound; any other one means a packet came
//   late, and `target` grows by one, up to TARGET_MAX (or the limits given).
// - More than `target` packets for TRIM_BLOCKS in a row, or more than MAX_PACKETS at all: drop
//   one packet, with a crossfade over it. This also takes up the clock drift when the clone's
//   clock runs faster than the sound card's; when it runs slower, the drift is an underrun.
// - DECAY_BLOCKS with no late underrun and at least 2 packets always waiting: `target` falls by
//   one, down to TARGET_MIN.
// Every start of the sound fades in over FADE_FRAMES. After an underrun the output goes from
// the last frame played down to 0 over FADE_FRAMES, so the gap does not click either. There is
// no fade-out before the queue runs dry: a queue that nearly runs dry and then gets its next
// packet in time is the normal case (the clone sends in 10.7 ms blocks), and a fade there made
// short dips in steady sound.
//
// Relative imports only: Vite builds the worklet without the app's path aliases.

/** 2 ms at 48 kHz. */
export const FADE_FRAMES = 96;
/** One packet (`wire::socket::AUDIO_FRAME_MS`, 10 ms) at 48 kHz. */
export const PACKET_FRAMES = 480;
export const TARGET_MIN = 2;
export const TARGET_MAX = 6;
export const MAX_PACKETS = TARGET_MAX + 2;

/** The range `target` moves in, in packets; the defaults are the ones above. A phone on Wi-Fi
 *  gets a wider one (`~/ng/session.ts`), as its packets come less evenly. */
export interface BufferLimits {
  min: number;
  max: number;
}
/** About 0.67 s of 128-frame blocks. */
export const TRIM_BLOCKS = 250;
/** About 5.3 s of 128-frame blocks. */
export const DECAY_BLOCKS = 2000;

/** One decoded packet: the left and right channel. */
export interface Packet {
  l: Float32Array;
  r: Float32Array;
}

export class SoundBuffer {
  /** Packets to play, oldest first, and the frames of the first one played already. */
  private queue: Packet[] = [];
  private off = 0;
  private readonly min: number;
  private readonly max: number;
  target: number;
  private playing = false;
  /** The quiet marker came, and no packet since: the packets that do not come are silence. */
  private quiet = false;
  private gain = 0;
  /** The last frame played, and the frames left in its ramp down to 0 after an underrun. */
  private lastL = 0;
  private lastR = 0;
  private tail = 0;
  /** Frames left in a crossfade over a dropped packet, and that packet's length. */
  private xfade = 0;
  private dropLen = 0;
  /** Blocks in a row with more than `target` packets waiting. */
  private over = 0;
  /** Blocks since a late underrun or since `target` last fell, and the fewest frames that
   *  waited at the start of one of them while the sound played. */
  private clean = 0;
  private fewest = Infinity;
  /** Late underruns and dropped packets, for the console. */
  late = 0;
  dropped = 0;

  constructor(limits: BufferLimits = { min: TARGET_MIN, max: TARGET_MAX }) {
    this.min = limits.min;
    this.max = limits.max;
    this.target = this.min;
  }

  push(p: Packet) {
    this.quiet = false;
    this.queue.push(p);
    if (this.queue.length > this.max + MAX_PACKETS - TARGET_MAX) this.drop();
  }

  /** The quiet marker: the clone stopped sending because its sound is silent. */
  setQuiet() {
    this.quiet = true;
  }

  /** A new stream, or the sound was off: what waits belongs to the old one. */
  reset() {
    this.queue = [];
    this.off = this.xfade = this.over = this.clean = 0;
    this.fewest = Infinity;
    this.playing = false;
    this.gain = this.tail = 0;
  }

  /** Frames waiting to be played. */
  private avail(): number {
    let n = -this.off;
    for (const p of this.queue) n += p.l.length;
    return n;
  }

  /** Move the read position `n` frames on. */
  private advance(n: number) {
    this.off += n;
    while (this.queue.length && this.off >= this.queue[0].l.length) {
      this.off -= this.queue[0].l.length;
      this.queue.shift();
    }
  }

  /** The frame `k` frames after the read position, as [left, right]; there must be one. */
  private at(k: number): [number, number] {
    let i = 0;
    k += this.off;
    while (k >= this.queue[i].l.length) k -= this.queue[i++].l.length;
    return [this.queue[i].l[k], this.queue[i].r[k]];
  }

  /** Drop one packet's length of sound, with a crossfade from before it to after it. Only when
   *  that much waits, and not inside another crossfade. */
  private drop() {
    if (this.xfade > 0) return;
    const len = this.queue[0].l.length;
    if (this.avail() < len + FADE_FRAMES) return;
    if (!this.playing) {
      this.advance(len);
    } else {
      this.xfade = FADE_FRAMES;
      this.dropLen = len;
    }
    this.dropped++;
  }

  /** Fill one block of output. */
  render(l: Float32Array, r: Float32Array) {
    if (!this.playing && (this.queue.length >= this.target || (this.quiet && this.queue.length))) {
      this.playing = true;
    }
    if (this.playing) this.steer();
    let avail = this.avail();
    for (let i = 0; i < l.length; i++) {
      if (!this.playing || avail === 0) {
        if (this.playing) this.underrun();
        const k = this.tail > 0 ? this.tail-- / FADE_FRAMES : 0;
        l[i] = this.lastL * k;
        r[i] = this.lastR * k;
        continue;
      }
      this.gain = Math.min(this.gain + 1 / FADE_FRAMES, 1);
      let [sl, sr] = this.at(0);
      if (this.xfade > 0) {
        const a = (FADE_FRAMES - this.xfade + 1) / FADE_FRAMES;
        const [bl, br] = this.at(this.dropLen);
        sl = sl * (1 - a) + bl * a;
        sr = sr * (1 - a) + br * a;
      }
      l[i] = this.lastL = sl * this.gain;
      r[i] = this.lastR = sr * this.gain;
      this.advance(1);
      avail--;
      if (this.xfade > 0 && --this.xfade === 0) {
        this.advance(this.dropLen);
        avail -= this.dropLen;
      }
    }
  }

  /** Once per block while the sound plays: trim a queue that stays long, and let `target` fall
   *  after a clean period. */
  private steer() {
    if (this.queue.length > this.target) {
      if (++this.over >= TRIM_BLOCKS) {
        this.drop();
        this.over = 0;
      }
    } else {
      this.over = 0;
    }
    this.fewest = Math.min(this.fewest, this.avail());
    if (++this.clean >= DECAY_BLOCKS) {
      if (this.target > this.min && this.fewest >= 2 * PACKET_FRAMES) this.target--;
      this.clean = 0;
      this.fewest = Infinity;
    }
  }

  private underrun() {
    this.playing = false;
    this.gain = 0;
    this.xfade = 0;
    this.tail = FADE_FRAMES;
    if (this.quiet) return;
    this.late++;
    this.target = Math.min(this.max, this.target + 1);
    this.clean = 0;
    this.fewest = Infinity;
  }
}
