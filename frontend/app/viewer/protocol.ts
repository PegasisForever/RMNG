// The media socket's binary messages (`wire::browser`: video, clipboard pieces, sound), the
// H.264 reframing the decoder needs for a video message (Annex B to avcC), and the messages
// between the page, its worker and its sound worklet.
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

/** The NAL units of an Annex-B access unit, each without its start code. The zero byte in front
 *  of a 4-byte start code is not part of the unit before it. */
export function annexbNals(au: Uint8Array): Uint8Array[] {
  const out: Uint8Array[] = [];
  let start = -1;
  for (let i = 0; i + 2 < au.length; i++) {
    if (au[i] !== 0 || au[i + 1] !== 0 || au[i + 2] !== 1) continue;
    if (start >= 0) {
      let end = i;
      while (end > start && au[end - 1] === 0) end--;
      if (end > start) out.push(au.subarray(start, end));
    }
    start = i + 3;
    i += 2;
  }
  if (start >= 0 && start < au.length) out.push(au.subarray(start));
  return out;
}

const nalType = (nal: Uint8Array) => nal[0] & 0x1f;
const NAL_SPS = 7;
const NAL_PPS = 8;
const NAL_AUD = 9;

/** The `avcC` record (ISO 14496-15) of the first SPS and PPS in an access unit: the decoder's
 *  `description`, with 4-byte NAL lengths. Null when the unit has no SPS or no PPS. */
export function avcDescription(au: Uint8Array): Uint8Array | null {
  const nals = annexbNals(au);
  const sps = nals.find((n) => nalType(n) === NAL_SPS);
  const pps = nals.find((n) => nalType(n) === NAL_PPS);
  if (!sps || !pps || sps.length < 4) return null;
  const out = new Uint8Array(11 + sps.length + pps.length);
  // Version 1, profile, constraint flags, level, 0xff (4-byte lengths), 0xe1 (one SPS).
  out.set([1, sps[1], sps[2], sps[3], 0xff, 0xe1, sps.length >> 8, sps.length & 0xff]);
  out.set(sps, 8);
  out.set([1, pps.length >> 8, pps.length & 0xff], 8 + sps.length);
  out.set(pps, 11 + sps.length);
  return out;
}

/** The WebCodecs codec string (`avc1.PPCCLL`: profile, constraint flags, level) of an `avcC`
 *  record. Safari refuses a stream whose real profile or level is above the configured one, so
 *  it comes from the stream's SPS and is never guessed. */
export const codecOf = (description: Uint8Array) =>
  `avc1.${hex(description[1])}${hex(description[2])}${hex(description[3])}`;

/** An Annex-B access unit as 4-byte-length-prefixed NAL units, the framing an `avcC`
 *  description implies, without its SPS, PPS (they are in the description) and delimiter.
 *  Every browser decodes this form; Safari decodes no other. */
export function annexbToAvcc(au: Uint8Array): Uint8Array<ArrayBuffer> {
  const nals = annexbNals(au).filter((n) => ![NAL_SPS, NAL_PPS, NAL_AUD].includes(nalType(n)));
  const out = new Uint8Array(nals.reduce((n, nal) => n + 4 + nal.length, 0));
  const v = new DataView(out.buffer);
  let at = 0;
  for (const nal of nals) {
    v.setUint32(at, nal.length);
    out.set(nal, at + 4);
    at += 4 + nal.length;
  }
  return out;
}

/** Whether two byte arrays (or two nulls) are equal. */
export function sameBytes(a: Uint8Array | null, b: Uint8Array | null): boolean {
  if (!a || !b) return a === b;
  return a.length === b.length && a.every((x, i) => x === b[i]);
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

/** What the page tells about its sound (`~/viewer/sound.ts`): the worklet's last report, and the
 *  AudioContext's rate and latencies (s). */
export interface SoundReport extends SoundStats {
  sampleRate: number;
  baseLatency: number;
  outputLatency: number;
}

/** Worker → page. */
export type WorkerOut =
  | { type: "message"; msg: ToBrowser }
  /** A clipboard reply, put together from its pieces, its bytes transferred. */
  | { type: "clipboard"; data: ClipboardBytes }
  | { type: "connected"; connected: boolean };
