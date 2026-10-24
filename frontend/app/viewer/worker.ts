// The browser viewer's worker: it owns the WebSocket, the H.264 decoder and the canvas, so a
// busy page thread never delays a frame. With sound on, it also decodes the sound packets
// (WebCodecs Opus) and sends the samples straight to the page's sound worklet on a port of
// their own, so a busy page thread never delays the sound either.
//
// Display: decoded frames are drawn straight onto the page's canvas, which the page gave to
// this worker (`transferControlToOffscreen`). That is one GPU copy per frame, and every part of
// it is plain, long-shipped Chrome API. A track generator into a <video> would save the copy,
// but the track would have to cross from this worker to the page, or the frames would have to
// go to the page thread first; both are less certain in Chrome than a canvas, so in Chrome the
// copy stays until a measurement shows it matters. Safari draws a VideoFrame on a worker canvas
// slowly (Selkies measured 5 to 25 ms a frame, at times over 100), so where the page asks for
// it and the worker has the standard `VideoTrackGenerator` (Safari 18 and later), the frames go
// into a generator's track instead, which the page shows in a <video> (`./track.ts`).
//
// H.264: the server sends Annex B. Safari's decoder takes only avcC framing (a `description`
// with the SPS and PPS, and length-prefixed NAL units; Selkies found this), which every browser
// takes, so each access unit is reframed here for all of them.
//
// Relative imports only: Vite builds the worker without the app's path aliases.
import type { FromBrowser } from "../lib/wire/FromBrowser";
import type { ToBrowser } from "../lib/wire/ToBrowser";
import { type HeldInput, freshInput, holdInput, letterbox } from "./input";
import {
  AUDIO_FRAME_MS,
  AUDIO_HEADER_LEN,
  type ClipboardBytes,
  KIND_AUDIO,
  KIND_VIDEO,
  type SoundIn,
  VIDEO_HEADER_LEN,
  type WorkerIn,
  type WorkerOut,
  RateMeter,
  clipboardPieces,
  annexbToAvcc,
  avcDescription,
  codecOf,
  isClipboardPiece,
  parseAudioHeader,
  parseClipboardPiece,
  parseVideoHeader,
  sameBytes,
} from "./protocol";

/** The standard track generator, a DedicatedWorker global (Safari 18 and later); not in
 *  TypeScript's DOM types. */
declare const VideoTrackGenerator:
  | (new () => { track: MediaStreamTrack; writable: WritableStream<VideoFrame> })
  | undefined;

const scope = self as unknown as Worker;
const post = (m: WorkerOut, transfer: Transferable[] = []) => scope.postMessage(m, transfer);

/** After a socket on which the server spoke closes, connect again at once; after each try that
 *  fails, wait this long, doubled each time up to RECONNECT_MAX_MS. */
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
/** Clipboard pieces on the way to the server stay within what the uplink carried in the lowest
 *  piece round trip and this long (at least one piece), so acks and input wait about this long
 *  behind an upload at most. */
const UPLOAD_WINDOW_MS = 100;

let url = "";
let monitor = 0;
let ctx: OffscreenCanvasRenderingContext2D;
let ws: WebSocket | null = null;
/** The wait before the next connect, when the last try failed. */
let reconnectMs = 0;
/** The last frame drawn, kept to draw again when the canvas changes size. */
let last: VideoFrame | null = null;
/** Where frames go instead of the canvas, when the page shows the video through a track. */
let track: WritableStreamDefaultWriter<VideoFrame> | null = null;

let decoder: VideoDecoder | null = null;
/** The codec the decoder is configured for, and its avcC description; "" and null until a
 *  keyframe with an SPS and a PPS arrives. */
let codec = "";
let description: Uint8Array | null = null;
/** The last chunk timestamp given. Each chunk gets a new, higher one, also across decoders:
 *  Safari gives frames wrong timestamps when chunks share one. */
let stamp = 0;
/** The chunks in the decoder, oldest first: their timestamp and frame number. */
let inDecoder: { stamp: number; seq: number }[] = [];
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
/** The replies this window asked for since the last copy (`serial mime`). A piece of anything
 *  else is the rest of a reply the window no longer wants. */
let wanted = new Set<string>();
/** Clipboard pieces waiting to be sent; the size and send time of those sent on this socket
 *  that the server has not said arrived (`clip_ack`), how many arrived, the rate they arrived
 *  at, and the lowest round trip of one. */
let upload: ArrayBuffer[] = [];
let upOut: { len: number; at: number }[] = [];
let upAcked = 0;
let upRate = new RateMeter();
let upFloor = Infinity;
/** Input made while the socket is down. */
let held: HeldInput = [];

/** Sound: whether the page wants it, the port to its worklet, the Opus decoder, and the stream
 *  the decoder is on (-1: none yet, so the next packet starts one). */
let soundOn = false;
let soundPort: MessagePort | null = null;
let audio: AudioDecoder | null = null;
let audioStream = -1;

function send(msg: FromBrowser) {
  if (msg.t === "clipboard" && msg.k === "offer") newCopy();
  if (msg.t === "clipboard" && msg.k === "request") wanted.add(`${msg.serial} ${msg.mime_type}`);
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
  send({
    t: "ack",
    seq,
    held_ms: newest ? Math.round(now - newest.at) : 0,
    clip: clipIn,
    decoding: decoder?.decodeQueueSize ?? 0,
  });
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
  description = null;
  inDecoder = [];
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

function configure(d: Uint8Array) {
  const config: VideoDecoderConfig = {
    codec: codecOf(d),
    description: d,
    optimizeForLatency: true,
    // The 4:2:0 encoder writes BT.709 matrix, limited range, and leaves the desktop's sRGB
    // transfer as it is; the stream carries no colour data. A BT.709 transfer here would make
    // dark areas grey.
    colorSpace: { primaries: "bt709", transfer: "iec61966-2-1", matrix: "bt709", fullRange: false },
    hardwareAcceleration: software ? "prefer-software" : "no-preference",
  };
  decoder!.configure(config);
  codec = config.codec;
  description = d;
}

function onVideo(buf: ArrayBuffer) {
  const h = parseVideoHeader(buf);
  const au = new Uint8Array(buf, VIDEO_HEADER_LEN);
  // A keyframe behind a backlog: drop the backlog, so the picture catches up at once.
  if (h.key && decoder!.decodeQueueSize > KEY_RESET_BACKLOG) resetDecoder();
  if (h.key) {
    // New parameter sets (a new monitor size, a new encoder) need a new configuration.
    const d = avcDescription(au);
    if (d && !sameBytes(d, description)) configure(d);
  }
  // A frame dropped while waiting for a keyframe (or for a keyframe with an SPS) is done when
  // it arrives; a decoded one when the decoder outputs it, so the server sees a slow decoder.
  if ((waitKey && !h.key) || !codec) return done(h.seq);
  waitKey = false;
  inDecoder.push({ stamp: ++stamp, seq: h.seq });
  decoder!.decode(
    new EncodedVideoChunk({
      type: h.key ? "key" : "delta",
      timestamp: stamp,
      data: annexbToAvcc(au),
    }),
  );
}

/** The frame number of the chunk a decoded frame came from. The stream has no B-frames, so
 *  frames come out in the order their chunks went in: a timestamp the browser changed (Safari
 *  can) still gets the oldest chunk's number. */
function seqOf(timestamp: number): number | undefined {
  const i = inDecoder.findIndex((c) => c.stamp === timestamp);
  return inDecoder.splice(0, i < 0 ? 1 : i + 1).at(-1)?.seq;
}

function onFrame(frame: VideoFrame) {
  const seq = seqOf(frame.timestamp);
  if (seq !== undefined) done(seq);
  if (track) return toTrack(frame);
  // A new monitor size leaves parts of the old picture in the bars.
  if (last?.displayWidth !== frame.displayWidth || last.displayHeight !== frame.displayHeight) {
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
  }
  last?.close();
  last = frame;
  draw();
}

/** Hand a frame to the page's <video>. The track keeps only a few: over that, the frame is
 *  dropped, as the newest frame replaces the last one on the canvas. A track that fails goes
 *  back to the canvas. */
function toTrack(frame: VideoFrame) {
  const w = track!;
  if (w.desiredSize !== null && w.desiredSize <= 0) return frame.close();
  // The write closes the frame, unless it fails.
  w.write(frame).catch(() => {
    frame.close();
    if (track !== w) return;
    track = null;
    post({ type: "track", track: null });
  });
}

/** Write frames to a track that the page shows, if the worker can make one. */
function startTrack() {
  if (typeof VideoTrackGenerator === "undefined") return;
  try {
    const gen = new VideoTrackGenerator();
    post({ type: "track", track: gen.track }, [gen.track]);
    track = gen.writable.getWriter();
  } catch (e) {
    console.warn("video track did not start, the canvas shows the video:", e);
  }
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

/** One piece of a clipboard reply from the server. Acknowledged at once: the server keeps only
 *  a few pieces on the way. */
function onClipboardPiece(buf: ArrayBuffer) {
  clipIn++;
  ack();
  const p = parseClipboardPiece(buf);
  const key = `${p.serial} ${p.mimeType}`;
  if (!wanted.has(key)) return;
  if (p.last) wanted.delete(key);
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

/** Send clipboard pieces while those on the way fit in the lowest round trip and
 *  UPLOAD_WINDOW_MS of the rate the server's `clip_ack`s show, and always one. (`bufferedAmount`
 *  does not see the data waiting in the kernel and on the link, which on a slow uplink held
 *  acks back for seconds.) */
function sendUpload() {
  const windowMs = (Number.isFinite(upFloor) ? upFloor : 0) + UPLOAD_WINDOW_MS;
  while (upload.length && ws?.readyState === WebSocket.OPEN) {
    const out = upOut.reduce((n, p) => n + p.len, 0) + upload[0].byteLength;
    if (upOut.length && out > upRate.rate * windowMs) return;
    const piece = upload.shift()!;
    ws.send(piece);
    upOut.push({ len: piece.byteLength, at: performance.now() });
  }
}

/** The server says `n` pieces of this socket arrived (wraps). */
function onClipAck(n: number) {
  const now = performance.now();
  for (let i = (n - upAcked) >>> 0; i > 0 && upOut.length; i--) {
    const p = upOut.shift()!;
    upRate.add(p.len, now);
    upFloor = Math.min(upFloor, now - p.at);
  }
  upAcked = n;
  sendUpload();
}

function queueUpload(d: ClipboardBytes) {
  upload.push(...clipboardPieces(d));
  sendUpload();
}

/** A newer copy, here or anywhere: the clipboard replies still on their way are stale. */
function newCopy() {
  upload = [];
  incoming = null;
  wanted = new Set();
}

const toWorklet = (m: SoundIn, transfer: Transferable[] = []) =>
  soundPort?.postMessage(m, transfer);

/** A new decoder, and nothing stale in the worklet. */
function resetAudio() {
  if (audio && audio.state !== "closed") audio.close();
  audio = new AudioDecoder({
    output: onPcm,
    error: (e) => {
      console.warn("sound decoder error:", e);
      audioStream = -1; // the next packet starts again
    },
  });
  audio.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 2 });
  toWorklet({ t: "reset" });
}

function onAudio(buf: ArrayBuffer) {
  if (!soundOn || !soundPort) return;
  const h = parseAudioHeader(buf);
  if (h.stream !== audioStream || audio?.state !== "configured") {
    resetAudio();
    audioStream = h.stream;
  }
  if (buf.byteLength === AUDIO_HEADER_LEN) return toWorklet({ t: "quiet" });
  audio!.decode(
    new EncodedAudioChunk({
      type: "key",
      timestamp: h.seq * AUDIO_FRAME_MS * 1000,
      data: new Uint8Array(buf, AUDIO_HEADER_LEN),
    }),
  );
}

function onPcm(d: AudioData) {
  // A packet still in the decoder when the sound went off.
  if (!soundOn) return d.close();
  const l = new Float32Array(d.numberOfFrames);
  const r = new Float32Array(d.numberOfFrames);
  d.copyTo(l, { planeIndex: 0, format: "f32-planar" });
  d.copyTo(r, { planeIndex: d.numberOfChannels > 1 ? 1 : 0, format: "f32-planar" });
  d.close();
  toWorklet({ t: "pcm", l, r }, [l.buffer, r.buffer]);
}

function setSound(on: boolean, port?: MessagePort) {
  if (port) soundPort = port;
  soundOn = on;
  audioStream = -1;
  toWorklet({ t: "reset" });
  send({ t: "sound", on });
}

function connect() {
  clipIn = upAcked = 0;
  upOut = [];
  upRate = new RateMeter();
  upFloor = Infinity;
  newCopy();
  ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    // No keyframe request: joining primes this monitor with a fresh keyframe. Frames still in
    // the old decoder belong to the old socket, and so does what it decoded until now.
    resetDecoder();
    newest = acked = null;
    // The server starts each socket with no sound.
    audioStream = -1;
    if (soundOn) send({ t: "sound", on: true });
    post({ type: "connected", connected: true });
    const now = performance.now();
    for (const h of freshInput(held, now)) send({ t: "input", ...h.msg });
    held = [];
  };
  ws.onmessage = (e: MessageEvent<ArrayBuffer | string>) => {
    if (typeof e.data === "string") {
      const msg = JSON.parse(e.data) as ToBrowser;
      // The server's first message: it is up, so a later close reconnects at once. (Reset on
      // open, a server that accepts and closes at once would be tried in a tight loop.)
      if (msg.t === "version") reconnectMs = 0;
      if (msg.t === "clip_ack") {
        onClipAck(msg.n);
        return;
      }
      if (msg.t === "clipboard" && msg.k === "offer") newCopy();
      post({ type: "message", msg });
    } else if (new Uint8Array(e.data, 0, 1)[0] === KIND_VIDEO) {
      onVideo(e.data);
    } else if (new Uint8Array(e.data, 0, 1)[0] === KIND_AUDIO) {
      onAudio(e.data);
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
  if (m.type === "sound") {
    setSound(m.on, m.port);
    return;
  }
  if (m.type === "resize") {
    // Setting the size clears the canvas.
    ctx.canvas.width = m.width;
    ctx.canvas.height = m.height;
    draw();
    return;
  }
  ({ monitor } = m);
  // This page's id, the same on each reconnect: the server replaces the old socket of this
  // window with the new one at once, which may still be open on a network that is gone.
  url = `${m.url}&tab=${crypto.randomUUID()}`;
  ctx = m.canvas.getContext("2d")!;
  if (m.track) startTrack();
  resetDecoder();
  connect();
  setInterval(ack, ACK_MS);
};
