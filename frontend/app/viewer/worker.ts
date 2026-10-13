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
import {
  VIDEO_HEADER_LEN,
  type ViewerStats,
  type WorkerInit,
  type WorkerOut,
  codecFromAnnexB,
  parseVideoHeader,
} from "./protocol";

const scope = self as unknown as Worker;
const post = (m: WorkerOut) => scope.postMessage(m);

/** Wait this long after the socket closes before connecting again. */
const RECONNECT_MS = 1000;
/** After this many decoder errors, ask Chrome for its software decoder. */
const SOFTWARE_AFTER_ERRORS = 3;

let url = "";
let monitor = 0;
let ctx: OffscreenCanvasRenderingContext2D;
let ws: WebSocket | null = null;

let decoder: VideoDecoder | null = null;
/** The codec the decoder is configured for; "" until a keyframe with an SPS arrives. */
let codec = "";
/** Drop frames until a keyframe: at start, after a reconnect and after a decoder error. */
let waitKey = true;
let errors = 0;
let decoderKind: ViewerStats["decoder"] = "unknown";

// Stats for the current second.
let frames = 0;
let decodeSum = 0;
let delaySum = 0;
let delayCount = 0;
let delayMin = Infinity;
/** Minimum delay of each of the last 10 seconds. */
const delayMins: number[] = [];
/** When `decode()` was called for each pending chunk, by chunk timestamp. */
const decodeStart = new Map<number, number>();

function send(msg: FromBrowser) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function requestKeyframe() {
  send({ t: "request_keyframe", monitor_id: monitor });
}

function resetDecoder() {
  if (decoder && decoder.state !== "closed") decoder.close();
  decoder = new VideoDecoder({ output: onFrame, error: onDecoderError });
  codec = "";
  waitKey = true;
  decodeStart.clear();
}

function onDecoderError(e: DOMException) {
  errors++;
  console.warn(`decoder error ${errors}:`, e);
  resetDecoder();
  requestKeyframe();
}

function configure(c: string) {
  const software = errors >= SOFTWARE_AFTER_ERRORS;
  const config: VideoDecoderConfig = {
    codec: c,
    optimizeForLatency: true,
    // The 4:2:0 encoder writes BT.709 limited range. Chrome ignores the stream's own colour
    // data, so say it here.
    colorSpace: { primaries: "bt709", transfer: "bt709", matrix: "bt709", fullRange: false },
    hardwareAcceleration: software ? "prefer-software" : "no-preference",
  };
  decoder!.configure(config);
  codec = c;
  decoderKind = "unknown";
  if (software) {
    decoderKind = "software";
  } else {
    // With "no-preference" Chrome takes a hardware decoder whenever it has one.
    void VideoDecoder.isConfigSupported({ ...config, hardwareAcceleration: "prefer-hardware" }).then(
      (r) => {
        if (codec === c) decoderKind = r.supported ? "hardware" : "software";
      },
      () => {},
    );
  }
}

function onVideo(buf: ArrayBuffer) {
  const h = parseVideoHeader(buf);
  const delay = performance.timeOrigin + performance.now() - h.serverUs / 1000;
  delaySum += delay;
  delayCount++;
  delayMin = Math.min(delayMin, delay);

  if (waitKey && !h.key) return;
  const au = new Uint8Array(buf, VIDEO_HEADER_LEN);
  if (h.key) {
    const c = codecFromAnnexB(au);
    if (c && c !== codec) configure(c);
  }
  if (!codec) return; // a keyframe without an SPS before any with one: wait for the next
  waitKey = false;
  decodeStart.set(h.serverUs, performance.now());
  decoder!.decode(
    new EncodedVideoChunk({ type: h.key ? "key" : "delta", timestamp: h.serverUs, data: au }),
  );
}

function onFrame(frame: VideoFrame) {
  const start = decodeStart.get(frame.timestamp);
  if (start !== undefined) {
    decodeSum += performance.now() - start;
    decodeStart.delete(frame.timestamp);
  }
  const canvas = ctx.canvas;
  if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
    canvas.width = frame.displayWidth;
    canvas.height = frame.displayHeight;
  }
  ctx.drawImage(frame, 0, 0);
  frame.close();
  frames++;
}

function connect() {
  ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    post({ type: "connected", connected: true });
    waitKey = true;
    requestKeyframe();
  };
  ws.onmessage = (e: MessageEvent<ArrayBuffer | string>) => {
    if (typeof e.data === "string") {
      post({ type: "message", msg: JSON.parse(e.data) as ToBrowser });
    } else {
      onVideo(e.data);
    }
  };
  ws.onclose = () => {
    ws = null;
    post({ type: "connected", connected: false });
    setTimeout(connect, RECONNECT_MS);
  };
}

function reportStats() {
  delayMins.push(delayMin);
  if (delayMins.length > 10) delayMins.shift();
  const floor = Math.min(...delayMins);
  post({
    type: "stats",
    stats: {
      fps: frames,
      decodeMs: frames ? decodeSum / frames : 0,
      delayMs: delayCount ? delaySum / delayCount - floor : 0,
      decoder: decoderKind,
    },
  });
  frames = 0;
  decodeSum = 0;
  delaySum = 0;
  delayCount = 0;
  delayMin = Infinity;
}

scope.onmessage = (e: MessageEvent<WorkerInit>) => {
  ({ url, monitor } = e.data);
  ctx = e.data.canvas.getContext("2d")!;
  resetDecoder();
  connect();
  setInterval(reportStats, 1000);
};
