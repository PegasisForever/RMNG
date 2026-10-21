// The browser viewer's binary messages (`wire::browser`: video, clipboard pieces, sound), the
// H.264 fact the decoder needs from a video message, and the messages between the viewer page,
// its worker and its sound worklet.
//
// The worker imports this file, so it uses relative imports only: Vite builds the worker
// without the app's path aliases.
import type { FromBrowser } from "../lib/wire/FromBrowser";
import type { ToBrowser } from "../lib/wire/ToBrowser";

/** Byte 0 of a binary message. A clipboard reply crosses in pieces: kind 2 while more follow,
 *  kind 1 for the last (or only) one. */
export const KIND_VIDEO = 0;
const KIND_CLIPBOARD_DATA = 1;
const KIND_CLIPBOARD_PIECE = 2;
export const KIND_AUDIO = 3;
/** Most bytes of a clipboard reply in one piece (`wire::browser::CLIPBOARD_PIECE_BYTES`). */
export const CLIPBOARD_PIECE_BYTES = 64 * 1024;

export const VIDEO_HEADER_LEN = 10;

interface VideoHeader {
  monitorId: number;
  /** Frame number from the monitor's encoder. */
  seq: number;
  key: boolean;
}

/** Read the header in front of a video message (big-endian; layout in `wire::browser`). */
export function parseVideoHeader(buf: ArrayBuffer): VideoHeader {
  const v = new DataView(buf);
  return {
    key: (v.getUint8(1) & 1) !== 0,
    monitorId: v.getUint32(2),
    seq: v.getUint32(6),
  };
}

export const AUDIO_HEADER_LEN = 9;
/** The length of one sound packet (`wire::socket::AUDIO_FRAME_MS`), in ms. */
export const AUDIO_FRAME_MS = 10;

interface AudioHeader {
  /** A new number when the sound comes from a new capture run: start the decoder again. */
  stream: number;
  /** Packet number in its stream. */
  seq: number;
}

/** Read the header in front of a sound message (big-endian; layout in `wire::browser`). An
 *  audio message with nothing after the header is the quiet marker: the clone's sound is
 *  silent, and it sends nothing until the sound comes back. */
export function parseAudioHeader(buf: ArrayBuffer): AudioHeader {
  const v = new DataView(buf);
  return { stream: v.getUint32(1), seq: v.getUint32(5) };
}

/** One clipboard reply, put together from its pieces (`wire::browser::encode_clipboard_piece`).
 *  Empty `bytes` means the owner could not read them. */
export interface ClipboardBytes {
  serial: number;
  mimeType: string;
  bytes: Uint8Array<ArrayBuffer>;
}

/** Whether a binary message is a clipboard piece. */
export const isClipboardPiece = (buf: ArrayBuffer) =>
  [KIND_CLIPBOARD_DATA, KIND_CLIPBOARD_PIECE].includes(new Uint8Array(buf, 0, 1)[0]);

/** One piece of a clipboard reply as a binary message: kind, serial (u64), MIME length (u16),
 *  MIME, the piece's bytes. `last` marks the reply's last piece. */
export function encodeClipboardPiece(d: ClipboardBytes, last: boolean): ArrayBuffer {
  const mime = new TextEncoder().encode(d.mimeType);
  const out = new Uint8Array(11 + mime.length + d.bytes.length);
  const v = new DataView(out.buffer);
  v.setUint8(0, last ? KIND_CLIPBOARD_DATA : KIND_CLIPBOARD_PIECE);
  v.setBigUint64(1, BigInt(d.serial));
  v.setUint16(9, mime.length);
  out.set(mime, 11);
  out.set(d.bytes, 11 + mime.length);
  return out.buffer;
}

/** A clipboard reply as the binary messages to send, in order. */
export function clipboardPieces(d: ClipboardBytes): ArrayBuffer[] {
  const out: ArrayBuffer[] = [];
  for (let at = 0; at === 0 || at < d.bytes.length; at += CLIPBOARD_PIECE_BYTES) {
    const bytes = d.bytes.subarray(at, at + CLIPBOARD_PIECE_BYTES);
    out.push(encodeClipboardPiece({ ...d, bytes }, at + CLIPBOARD_PIECE_BYTES >= d.bytes.length));
  }
  return out;
}

/** The rate at which bytes arrive, in bytes per ms, over the last complete period of
 *  `periodMs`; 0 until one is complete. */
export class RateMeter {
  rate = 0;
  private start = -1;
  private bytes = 0;

  constructor(private periodMs = 500) {}

  add(bytes: number, now: number) {
    if (this.start < 0) this.start = now;
    this.bytes += bytes;
    if (now - this.start >= this.periodMs) {
      this.rate = this.bytes / (now - this.start);
      this.start = now;
      this.bytes = 0;
    }
  }
}

/** Read a binary message written by `encodeClipboardPiece` (or by the server). The bytes are a
 *  view into `buf`, not a copy. */
export function parseClipboardPiece(buf: ArrayBuffer): ClipboardBytes & { last: boolean } {
  const v = new DataView(buf);
  const n = v.getUint16(9);
  return {
    serial: Number(v.getBigUint64(1)),
    mimeType: new TextDecoder().decode(new Uint8Array(buf, 11, n)),
    bytes: new Uint8Array(buf, 11 + n),
    last: v.getUint8(0) === KIND_CLIPBOARD_DATA,
  };
}

const hex = (b: number) => b.toString(16).padStart(2, "0");

/** The WebCodecs codec string (`avc1.PPCCLL`: profile, constraint flags, level) from the SPS in
 *  an Annex-B access unit, or null when the unit has no SPS. Stops at the first slice, because
 *  the SPS comes before it, so a large frame is not scanned to its end. */
export function codecFromAnnexB(au: Uint8Array): string | null {
  for (let i = 0; i + 3 < au.length; i++) {
    // A 4-byte start code ends in the same three bytes as a 3-byte one.
    if (au[i] !== 0 || au[i + 1] !== 0 || au[i + 2] !== 1) continue;
    const type = au[i + 3] & 0x1f;
    if (type === 7 && i + 6 < au.length) {
      return `avc1.${hex(au[i + 4])}${hex(au[i + 5])}${hex(au[i + 6])}`;
    }
    if (type === 1 || type === 5) return null;
    i += 3;
  }
  return null;
}

/** Page → worker: first the canvas to draw on and the socket to read (`init`, once), then a
 *  message to send on the socket (`send`: input waits while the socket is down, the rest is
 *  dropped), a clipboard reply to send in pieces (`clipboard`, its bytes transferred), the
 *  canvas's new size in device pixels (`resize`), or sound on or off (`sound`; with `on`, the
 *  port to the sound worklet the first time). */
export type WorkerIn =
  | { type: "init"; canvas: OffscreenCanvas; url: string; monitor: number }
  | { type: "send"; msg: FromBrowser }
  | { type: "clipboard"; data: ClipboardBytes }
  | { type: "resize"; width: number; height: number }
  | { type: "sound"; on: boolean; port?: MessagePort };

/** Worker → sound worklet: one decoded packet (`pcm`, its channels transferred), the quiet
 *  marker, or `reset`: a new stream, or the sound was off, so what waits is stale. */
export type SoundIn =
  | { t: "pcm"; l: Float32Array; r: Float32Array }
  | { t: "quiet" }
  | { t: "reset" };

/** Sound worklet → page, once a second: the buffer's target (packets), and how many late
 *  underruns and dropped packets so far. */
export interface SoundStats {
  target: number;
  late: number;
  dropped: number;
}

/** Worker → page. */
export type WorkerOut =
  | { type: "message"; msg: ToBrowser }
  /** A clipboard reply, put together from its pieces, its bytes transferred. */
  | { type: "clipboard"; data: ClipboardBytes }
  | { type: "connected"; connected: boolean };
