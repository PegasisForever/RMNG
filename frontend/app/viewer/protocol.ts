// The browser viewer's binary messages (`wire::browser`: video, clipboard data), the H.264 fact
// the decoder needs from a video message, and the messages between the viewer page and its
// worker.
//
// The worker imports this file, so it uses relative imports only: Vite builds the worker
// without the app's path aliases.
import type { FromBrowser } from "../lib/wire/FromBrowser";
import type { ToBrowser } from "../lib/wire/ToBrowser";

/** Byte 0 of a binary message. */
export const KIND_VIDEO = 0;
const KIND_CLIPBOARD_DATA = 1;

export const VIDEO_HEADER_LEN = 18;

interface VideoHeader {
  monitorId: number;
  /** Frame number from the monitor's encoder. */
  seq: number;
  key: boolean;
  /** When the server's encoder produced the frame, microseconds since the Unix epoch. */
  serverUs: number;
}

/** Read the header in front of a video message (big-endian; layout in `wire::browser`). */
export function parseVideoHeader(buf: ArrayBuffer): VideoHeader {
  const v = new DataView(buf);
  return {
    key: (v.getUint8(1) & 1) !== 0,
    monitorId: v.getUint32(2),
    seq: v.getUint32(6),
    serverUs: Number(v.getBigUint64(10)),
  };
}

/** One clipboard reply (`wire::browser::encode_clipboard_data`). Empty `bytes` means the owner
 *  could not read them. */
export interface ClipboardBytes {
  serial: number;
  mimeType: string;
  bytes: Uint8Array<ArrayBuffer>;
}

/** Clipboard bytes as a binary message: kind, serial (u64), MIME length (u16), MIME, bytes. */
export function encodeClipboardData(d: ClipboardBytes): ArrayBuffer {
  const mime = new TextEncoder().encode(d.mimeType);
  const out = new Uint8Array(11 + mime.length + d.bytes.length);
  const v = new DataView(out.buffer);
  v.setUint8(0, KIND_CLIPBOARD_DATA);
  v.setBigUint64(1, BigInt(d.serial));
  v.setUint16(9, mime.length);
  out.set(mime, 11);
  out.set(d.bytes, 11 + mime.length);
  return out.buffer;
}

/** Read a binary message written by `encodeClipboardData` (or by the server). The bytes are a
 *  view into `buf`, not a copy. */
export function parseClipboardData(buf: ArrayBuffer): ClipboardBytes {
  const v = new DataView(buf);
  const n = v.getUint16(9);
  return {
    serial: Number(v.getBigUint64(1)),
    mimeType: new TextDecoder().decode(new Uint8Array(buf, 11, n)),
    bytes: new Uint8Array(buf, 11 + n),
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
 *  message to send on the socket (`send`, or `binary` for a transferred binary message such as
 *  `encodeClipboardData`; both dropped while the socket is closed), or the canvas's new size
 *  in device pixels (`resize`). */
export type WorkerIn =
  | { type: "init"; canvas: OffscreenCanvas; url: string; monitor: number }
  | { type: "send"; msg: FromBrowser }
  | { type: "binary"; buf: ArrayBuffer }
  | { type: "resize"; width: number; height: number };

export interface ViewerStats {
  /** Frames drawn in the last second. */
  fps: number;
  /** Mean time from `decode()` to the decoded frame, ms. */
  decodeMs: number;
  /** Mean (arrival − server time) in the last second, minus its minimum over the last 10 s, ms.
   *  The two clocks differ by an unknown offset; subtracting the minimum removes it. */
  delayMs: number;
  /** Chrome does not say which decoder runs. "default" is Chrome's own choice; "software"
   *  means the viewer asked for the software decoder after repeated decoder errors. */
  decoder: "default" | "software";
}

/** Worker → page. */
export type WorkerOut =
  | { type: "message"; msg: ToBrowser }
  /** A binary message that is not video (clipboard data), transferred. */
  | { type: "binary"; buf: ArrayBuffer }
  | { type: "connected"; connected: boolean }
  | { type: "stats"; stats: ViewerStats };
