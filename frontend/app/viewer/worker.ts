// The browser viewer's worker: it owns the WebSocket, the H.264 decoder and the canvas, so a
// busy page thread never delays a frame.
//
// Display: decoded frames are drawn straight onto the page's canvas, which the page gave to
// this worker (`transferControlToOffscreen`). That is one GPU copy per frame, and every part of
// it is plain, long-shipped Chrome API. A track generator into a <video> would save the copy,
// but the track would have to cross from this worker to the page, or the frames would have to
// go to the page thread first; both are less certain in Chrome than a canvas, so the copy stays
// until a measurement shows it matters.
//
// Relative imports only: Vite builds the worker without the app's path aliases.
import type { FromBrowser } from "../lib/wire/FromBrowser";
import type { ToBrowser } from "../lib/wire/ToBrowser";
import { type HeldInput, freshInput, holdInput, letterbox } from "./input";
import {
  type ClipboardBytes,
  KIND_VIDEO,
  VIDEO_HEADER_LEN,
  type WorkerIn,
  type WorkerOut,
  clipboardPieces,
  codecFromAnnexB,
  isClipboardPiece,
  parseClipboardPiece,
  parseVideoHeader,
} from "./protocol";

const scope = self as unknown as Worker;
const post = (m: WorkerOut, transfer: Transferable[] = []) => scope.postMessage(m, transfer);

/** After a socket that was open closes, connect again at once; after each try that fails, wait
 *  this long, doubled each time up to RECONNECT_MAX_MS. */
const RECONNECT_FIRST_MS = 400;
const RECONNECT_MAX_MS = 1000;
/** After this many decoder errors, each within ERROR_WINDOW_MS of the one before, use Chrome's
 *  software decoder for the rest of the page's life. */
const SOFTWARE_AFTER_ERRORS = 3;
const ERROR_WINDOW_MS = 5000;
/** A keyframe that finds more chunks than this waiting in the decoder resets the decoder and is
 *  decoded alone, so the picture catches up at once. */
const KEY_RESET_BACKLOG = 6;
/** Acknowledge the newest decoded frame this often when it changed, and every
 *  ACK_HEARTBEAT_MS when it did not, also before the first frame. The server takes the round
 *  trip and how far behind this window is from it, and closes a socket whose frames stay
 *  unacknowledged for 10 s, or that sends nothing for 10 s. */
const ACK_MS = 50;
const ACK_HEARTBEAT_MS = 1000;

let url = "";
let monitor = 0;
let ctx: OffscreenCanvasRenderingContext2D;
let ws: WebSocket | null = null;
/** The wait before the next connect, when the last try failed. */
let reconnectMs = 0;
/** The last frame drawn, kept to draw again when the canvas changes size. */
let last: VideoFrame | null = null;

let decoder: VideoDecoder | null = null;
/** The codec the decoder is configured for; "" until a keyframe with an SPS arrives. */
let codec = "";
/** Drop frames until a keyframe: at start, after a reconnect and after a decoder error. */
let waitKey = true;
/** Decoder errors in the current run (errors less than ERROR_WINDOW_MS apart). */
let errors = 0;
let lastErrorAt = -Infinity;
/** Set once the errors reach SOFTWARE_AFTER_ERRORS; never cleared. A decoder that decodes one
 *  keyframe and then fails again would otherwise never get here. */
let software = false;

/** The newest frame done on this socket (decoded, or dropped while waiting for a keyframe) and
 *  when; the last acknowledgement and when. */
let newest: { seq: number; at: number } | null = null;
let acked: { seq: number; clip: number; at: number } | null = null;
/** Clipboard pieces received on this socket; the reply they are putting together. */
let clipIn = 0;
let incoming: { serial: number; mimeType: string; parts: Uint8Array[] } | null = null;
/** Clipboard pieces waiting to be sent; those sent on this socket, and those the server says
 *  arrived (`clip_ack`). */
let upload: ArrayBuffer[] = [];
let upSent = 0;
let upAcked = 0;
/** Input made while the socket is down. */
let held: HeldInput = [];

function send(msg: FromBrowser) {
  if (msg.t === "clipboard" && msg.k === "offer") newCopy();
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
  else if (msg.t === "input") held = holdInput(held, msg, performance.now());
}

/** `held_ms` is how long the frame number waited here, so the server can take it off the
 *  round trip. Before the first frame (a terminal, or no video yet) the heartbeat names frame
 *  0, which the server ignores: it only shows that this window still reads. */
function ack() {
  const now = performance.now();
  const seq = newest?.seq ?? 0;
  if (acked?.seq === seq && acked.clip === clipIn && now - acked.at < ACK_HEARTBEAT_MS) return;
  send({ t: "ack", seq, held_ms: newest ? Math.round(now - newest.at) : 0, clip: clipIn });
  acked = { seq, clip: clipIn, at: now };
}

/** `seq` is done: decoded, or dropped while waiting for a keyframe. */
function done(seq: number) {
  newest = { seq, at: performance.now() };
}

function requestKeyframe() {
  send({ t: "request_keyframe", monitor_id: monitor });
}

function resetDecoder() {
  if (decoder && decoder.state !== "closed") decoder.close();
  decoder = new VideoDecoder({ output: onFrame, error: onDecoderError });
  codec = "";
  waitKey = true;
}

function onDecoderError(e: DOMException) {
  const now = performance.now();
  if (now - lastErrorAt > ERROR_WINDOW_MS) errors = 0;
  lastErrorAt = now;
  errors++;
  if (errors >= SOFTWARE_AFTER_ERRORS) software = true;
  console.warn(`decoder error ${errors}:`, e);
  resetDecoder();
  requestKeyframe();
}

function configure(c: string) {
  const config: VideoDecoderConfig = {
    codec: c,
    optimizeForLatency: true,
    // The 4:2:0 encoder writes BT.709 matrix, limited range, and leaves the desktop's sRGB
    // transfer as it is; the stream carries no colour data. A BT.709 transfer here would make
    // dark areas grey.
    colorSpace: { primaries: "bt709", transfer: "iec61966-2-1", matrix: "bt709", fullRange: false },
    hardwareAcceleration: software ? "prefer-software" : "no-preference",
  };
  decoder!.configure(config);
  codec = c;
}

function onVideo(buf: ArrayBuffer) {
  const h = parseVideoHeader(buf);
  const au = new Uint8Array(buf, VIDEO_HEADER_LEN);
  // A keyframe behind a backlog: drop the backlog, so the picture catches up at once.
  if (h.key && decoder!.decodeQueueSize > KEY_RESET_BACKLOG) resetDecoder();
  if (h.key) {
    const c = codecFromAnnexB(au);
    if (c && c !== codec) configure(c);
  }
  // A frame dropped while waiting for a keyframe (or for a keyframe with an SPS) is done when
  // it arrives; a decoded one when the decoder outputs it, so the server sees a slow decoder.
  if ((waitKey && !h.key) || !codec) return done(h.seq);
  waitKey = false;
  decoder!.decode(
    new EncodedVideoChunk({ type: h.key ? "key" : "delta", timestamp: h.seq, data: au }),
  );
}

function onFrame(frame: VideoFrame) {
  done(frame.timestamp);
  // A new monitor size leaves parts of the old picture in the bars.
  if (last?.displayWidth !== frame.displayWidth || last.displayHeight !== frame.displayHeight) {
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
  }
  last?.close();
  last = frame;
  draw();
}

/** Draw the last frame scaled to fit, at the canvas's device pixels: when the canvas has the
 *  monitor's size, each monitor pixel is one screen pixel. */
function draw() {
  if (!last) return;
  const w = last.displayWidth;
  const h = last.displayHeight;
  const { scale, offX, offY } = letterbox(ctx.canvas.width, ctx.canvas.height, w, h);
  ctx.drawImage(last, offX, offY, w * scale, h * scale);
}

/** One piece of a clipboard reply from the server. Acknowledged at once: the server sends the
 *  next piece only when this one arrived. */
function onClipboardPiece(buf: ArrayBuffer) {
  clipIn++;
  ack();
  const p = parseClipboardPiece(buf);
  if (incoming?.serial !== p.serial || incoming.mimeType !== p.mimeType) {
    incoming = { serial: p.serial, mimeType: p.mimeType, parts: [] };
  }
  incoming.parts.push(p.bytes);
  if (!p.last) return;
  const bytes = new Uint8Array(incoming.parts.reduce((n, b) => n + b.length, 0));
  let at = 0;
  for (const b of incoming.parts) {
    bytes.set(b, at);
    at += b.length;
  }
  incoming = null;
  post({ type: "clipboard", data: { serial: p.serial, mimeType: p.mimeType, bytes } }, [
    bytes.buffer,
  ]);
}

/** Send the next clipboard piece once the one before it arrived, so acks and input wait
 *  behind one piece at most. (`bufferedAmount` does not see the data waiting in the kernel
 *  and on the link, which on a slow uplink held acks back for seconds.) */
function sendUpload() {
  if (upload.length && upSent === upAcked && ws?.readyState === WebSocket.OPEN) {
    ws.send(upload.shift()!);
    upSent++;
  }
}

function queueUpload(d: ClipboardBytes) {
  upload.push(...clipboardPieces(d));
  sendUpload();
}

/** A newer copy, here or anywhere: the clipboard replies still on their way are stale. */
function newCopy() {
  upload = [];
  incoming = null;
}

function connect() {
  newest = acked = null;
  clipIn = upSent = upAcked = 0;
  newCopy();
  ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    reconnectMs = 0;
    // No keyframe request: joining primes this monitor with a fresh keyframe. Frames still in
    // the old decoder belong to the old socket.
    resetDecoder();
    post({ type: "connected", connected: true });
    const now = performance.now();
    for (const h of freshInput(held, now)) send({ t: "input", ...h.msg });
    held = [];
  };
  ws.onmessage = (e: MessageEvent<ArrayBuffer | string>) => {
    if (typeof e.data === "string") {
      const msg = JSON.parse(e.data) as ToBrowser;
      if (msg.t === "clip_ack") {
        upAcked = msg.n;
        sendUpload();
        return;
      }
      if (msg.t === "clipboard" && msg.k === "offer") newCopy();
      post({ type: "message", msg });
    } else if (new Uint8Array(e.data, 0, 1)[0] === KIND_VIDEO) {
      onVideo(e.data);
    } else if (isClipboardPiece(e.data)) {
      onClipboardPiece(e.data);
    }
  };
  ws.onclose = () => {
    ws = null;
    post({ type: "connected", connected: false });
    setTimeout(connect, reconnectMs);
    reconnectMs = Math.min(reconnectMs * 2 || RECONNECT_FIRST_MS, RECONNECT_MAX_MS);
  };
}

scope.onmessage = (e: MessageEvent<WorkerIn>) => {
  const m = e.data;
  if (m.type === "send") {
    send(m.msg);
    return;
  }
  if (m.type === "clipboard") {
    if (ws?.readyState === WebSocket.OPEN) queueUpload(m.data);
    return;
  }
  if (m.type === "resize") {
    // Setting the size clears the canvas.
    ctx.canvas.width = m.width;
    ctx.canvas.height = m.height;
    draw();
    return;
  }
  ({ url, monitor } = m);
  ctx = m.canvas.getContext("2d")!;
  resetDecoder();
  connect();
  setInterval(ack, ACK_MS);
};
