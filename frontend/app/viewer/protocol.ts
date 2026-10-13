// The browser viewer's binary video message (`wire::browser::VideoHeader`), the H.264 fact the
// decoder needs from it, and the messages between the viewer page and its worker.
//
// The worker imports this file, so it uses relative imports only: Vite builds the worker
// without the app's path aliases.
import type { ToBrowser } from "../lib/wire/ToBrowser";

export const VIDEO_HEADER_LEN = 18;

export interface VideoHeader {
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

/** Page → worker, once: the canvas to draw on and the socket to read. */
export interface WorkerInit {
  canvas: OffscreenCanvas;
  url: string;
  monitor: number;
}

export interface ViewerStats {
  /** Frames drawn in the last second. */
  fps: number;
  /** Mean time from `decode()` to the decoded frame, ms. */
  decodeMs: number;
  /** Mean (arrival − server time) in the last second, minus its minimum over the last 10 s, ms.
   *  The two clocks differ by an unknown offset; subtracting the minimum removes it. */
  delayMs: number;
  /** Chrome does not say which decoder runs. "hardware" means Chrome supports a hardware
   *  decoder for this stream and the viewer did not fall back to software. */
  decoder: "hardware" | "software" | "unknown";
}

/** Worker → page. */
export type WorkerOut =
  | { type: "message"; msg: ToBrowser }
  | { type: "connected"; connected: boolean }
  | { type: "stats"; stats: ViewerStats };
