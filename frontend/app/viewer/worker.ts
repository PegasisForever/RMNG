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
import { letterbox } from "./input";
import {
  KIND_VIDEO,
  VIDEO_HEADER_LEN,
  type WorkerIn,
  type WorkerOut,
  codecFromAnnexB,
  parseVideoHeader,
} from "./protocol";

const scope = self as unknown as Worker;
const post = (m: WorkerOut, transfer: Transferable[] = []) => scope.postMessage(m, transfer);

/** Wait this long after the socket closes before connecting again. */
const RECONNECT_MS = 1000;
/** After this many decoder errors, each within ERROR_WINDOW_MS of the one before, use Chrome's
 *  software decoder for the rest of the page's life. */
const SOFTWARE_AFTER_ERRORS = 3;
const ERROR_WINDOW_MS = 5000;
/** More chunks than this waiting in the decoder: drop until the next keyframe to catch up. */
const DECODE_BACKLOG_MAX = 8;
/** Acknowledge the newest received frame this often when it changed, and every
 *  ACK_HEARTBEAT_MS when it did not. The server takes the round trip and how far behind this
 *  window is from it, and closes a socket whose frames stay unacknowledged for 10 s. */
const ACK_MS = 50;
const ACK_HEARTBEAT_MS = 1000;

let url = "";
let monitor = 0;
let ctx: OffscreenCanvasRenderingContext2D;
let ws: WebSocket | null = null;
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

/** The newest frame received on this socket and when; the last acknowledged one and when. */
let newest: { seq: number; at: number } | null = null;
let acked: { seq: number; at: number } | null = null;

function send(msg: FromBrowser) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

/** `held_ms` is how long the frame number waited here, so the server can take it off the
 *  round trip. */
function ack() {
  if (!newest) return;
  const now = performance.now();
  if (acked?.seq === newest.seq && now - acked.at < ACK_HEARTBEAT_MS) return;
  send({ t: "ack", seq: newest.seq, held_ms: Math.round(now - newest.at) });
  acked = { seq: newest.seq, at: now };
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
  // Received is what counts, decoded or not.
  newest = { seq: h.seq, at: performance.now() };
  if (waitKey && !h.key) return;
  const au = new Uint8Array(buf, VIDEO_HEADER_LEN);
  if (h.key) {
    const c = codecFromAnnexB(au);
    if (c && c !== codec) configure(c);
  }
  if (!codec) return; // a keyframe without an SPS before any with one: wait for the next
  // Never drop the keyframe itself: it is the frame that ends the wait. Ask for one: the
  // server sends keyframes only on request.
  if (!h.key && decoder!.decodeQueueSize > DECODE_BACKLOG_MAX) {
    waitKey = true;
    requestKeyframe();
    return;
  }
  waitKey = false;
  decoder!.decode(
    new EncodedVideoChunk({ type: h.key ? "key" : "delta", timestamp: h.seq, data: au }),
  );
}

function onFrame(frame: VideoFrame) {
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

function connect() {
  newest = acked = null;
  ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    // No keyframe request: joining primes this monitor with a fresh keyframe.
    post({ type: "connected", connected: true });
    waitKey = true;
  };
  ws.onmessage = (e: MessageEvent<ArrayBuffer | string>) => {
    if (typeof e.data === "string") {
      post({ type: "message", msg: JSON.parse(e.data) as ToBrowser });
    } else if (new Uint8Array(e.data, 0, 1)[0] === KIND_VIDEO) {
      onVideo(e.data);
    } else {
      post({ type: "binary", buf: e.data }, [e.data]);
    }
  };
  ws.onclose = () => {
    ws = null;
    post({ type: "connected", connected: false });
    setTimeout(connect, RECONNECT_MS);
  };
}

scope.onmessage = (e: MessageEvent<WorkerIn>) => {
  const m = e.data;
  if (m.type === "send") {
    send(m.msg);
    return;
  }
  if (m.type === "binary") {
    if (ws?.readyState === WebSocket.OPEN) ws.send(m.buf);
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
