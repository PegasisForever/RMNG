// The media socket's worker (`~/desk/session.ts`): it owns the WebSocket, the H.264 decoder and
// the canvas, so a busy page thread never delays a frame. With sound on, it also decodes the
// sound packets (Opus: WebCodecs, or libopus in WebAssembly, `./opus.ts`) and sends the samples
// straight to the page's sound worklet on a port of their own, so a busy page thread never
// delays the sound either.
//
// Display: decoded frames are drawn straight onto the page's canvas, which the page gave to
// this worker (`transferControlToOffscreen`). That is one GPU copy per frame, and every part of
// it is plain, long-shipped Chrome API. A track generator into a <video> would save the copy,
// but the track would have to cross from this worker to the page, or the frames would have to
// go to the page thread first; both are less certain in Chrome than a canvas, so the copy stays
// until a measurement shows it matters.
//
// H.264: the server sends Annex B. Safari's decoder takes only avcC framing (a `description`
// with the SPS and PPS, and length-prefixed NAL units; Selkies found this), which every browser
// takes, so each access unit is reframed here for all of them.
//
// Relative imports only: Vite builds the worker without the app's path aliases.
import type { FromBrowser } from "../lib/wire/FromBrowser";
import type { ToBrowser } from "../lib/wire/ToBrowser";
import { type HeldInput, freshInput, holdInput, letterbox } from "./input";
import { type Opus, openOpus } from "./opus";
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

const scope = self as unknown as Worker;
const post = (m: WorkerOut, transfer: Transferable[] = []) => scope.postMessage(m, transfer);

/** After a socket on which the server spoke closes, connect again at once; after each try that
 *  fails, wait this long, doubled each time up to RECONNECT_MAX_MS. Short: while the network
 *  comes back (a laptop waking up) each try fails fast, and the first after it should come soon. */
const RECONNECT_FIRST_MS = 250;
const RECONNECT_MAX_MS = 500;
/** Liveness. The server answers an `echo` ahead of the video in its queue; the browser sees no
 *  WebSocket ping. A socket that hears nothing for STALE_MS after an echo is half-open (a
 *  Wi-Fi drop with no sign of it: no error, nothing arrives), so it is dropped and a new one
 *  opened at once. STALE_MS leaves room for one large keyframe on a slow link: a message is
 *  heard only when all of it has arrived. */
const ECHO_MS = 1000;
const STALE_MS = 6000;
/** A socket not open in 3 times the last connect's time (within these limits; the longest
 *  before the first connect) is given up and a new one opened: one started while the network
 *  was gone (a laptop waking up, a tunnel such as Tailscale whose packets go nowhere) can wait
 *  for good, and Chrome starts no other socket to the server while one is connecting. Every
 *  second try in a row that times out gets twice as long, for a link slower than it was. */
const CONNECT_MIN_MS = 1500;
const CONNECT_MAX_MS = 8000;
/** The liveness check's interval; a tick this much later than due means the machine slept.
 *  Wall-clock time: on a Mac `performance.now()` stops while the machine sleeps. */
const LIVE_MS = 250;
const SLEPT_MS = 3000;
/** A wake within this long of the socket's start is the same wake (the page's events and the
 *  clock come together): the socket is not replaced again. */
const WAKE_AGAIN_MS = 1000;
/** After this many decoder errors, each within ERROR_WINDOW_MS of the one before, use Chrome's
 *  software decoder for the rest of the page's life. */
const SOFTWARE_AFTER_ERRORS = 3;
const ERROR_WINDOW_MS = 5000;
/** Chunks in the decoder this long with no frame out and no chunk in: the decoder holds frames
 *  until more come (Android's hardware decoders do), and on a still screen no more come. The
 *  worker then takes the software decoder, or, where there is none or it holds frames too,
 *  flushes the frames out each time the input stops (`checkHeld`). */
const HELD_MS = 250;
/** After a flush the next keyframe is asked for at most this often. */
const KEY_ASK_MS = 500;
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
/** The wait before the next connect, when the last try failed, and its timer. */
let reconnectMs = 0;
let retry: ReturnType<typeof setTimeout> | undefined;
/** Liveness (wall-clock ms): when the socket was started, when it last heard anything, the
 *  time by which it must hear something (Infinity: nothing asked), the last echo sent, how long
 *  the last connect took (null: none yet), and the connects in a row that timed out. */
let startedAt = 0;
let heardAt = 0;
let deadline = Infinity;
let echoAt = 0;
let echoId = 0;
let connectTook: number | null = null;
let timeouts = 0;
/** The last frame drawn, kept to draw again when the canvas changes size. */
let last: VideoFrame | null = null;

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
/** Set once the errors reach SOFTWARE_AFTER_ERRORS, or the decoder held frames; never cleared.
 *  A decoder that decodes one keyframe and then fails again would otherwise never get here. */
let software = false;
/** When the last chunk went into the decoder and the last frame came out. */
let lastIn = 0;
let lastOut = 0;
/** The decoder (the software one too, or there is none) holds frames: flush when input stops.
 *  `flushing` while a flush runs; `flushed` until the keyframe it needs arrives, and when it
 *  was last asked for. */
let flushWhenStill = false;
let flushing = false;
let flushed = false;
let keyAskedAt = -Infinity;

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
let audio: Opus | null = null;
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
  flushing = flushed = false;
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

/** Each ACK_MS: chunks held in the decoder with nothing in or out for HELD_MS. The first time,
 *  the software decoder takes over, where the browser has one; after that (or with none) the
 *  held frames are flushed out, and the next keyframe is waited for, as a flush requires. */
function checkHeld() {
  const now = performance.now();
  if (flushing || decoder?.state !== "configured" || !inDecoder.length) return;
  if (now - lastIn < HELD_MS || now - lastOut < HELD_MS) return;
  if (!software && !flushWhenStill) {
    void softwareDecodes().then((yes) => {
      if (yes) software = true;
      else flushWhenStill = true;
      if (yes) {
        resetDecoder();
        requestKeyframe();
      } else flush();
    });
    // Not again while the answer is on its way
    lastIn = now;
    return;
  }
  flushWhenStill = true;
  flush();
}

/** Whether the browser has a software decoder for the stream. */
async function softwareDecodes(): Promise<boolean> {
  if (!codec || !description) return false;
  try {
    const s = await VideoDecoder.isConfigSupported({ codec, description, hardwareAcceleration: "prefer-software" });
    return s.supported === true;
  } catch {
    return false;
  }
}

/** Get the frames the decoder holds out; the next chunk it takes must then be a keyframe. */
function flush() {
  const d = decoder!;
  flushing = true;
  waitKey = flushed = true;
  d.flush()
    .catch(() => {})
    .finally(() => {
      if (decoder === d) flushing = false;
    });
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
  if ((waitKey && !h.key) || !codec) {
    // A flush needs a keyframe after it: ask for one when the picture changes again
    const now = performance.now();
    if (flushed && now - keyAskedAt >= KEY_ASK_MS) {
      keyAskedAt = now;
      requestKeyframe();
    }
    return done(h.seq);
  }
  waitKey = flushed = false;
  lastIn = performance.now();
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
  lastOut = performance.now();
  const seq = seqOf(frame.timestamp);
  if (seq !== undefined) done(seq);
  // A new monitor size leaves parts of the old picture in the bars.
  // The page sizes the canvas to it (it can come before or after the view spec's size).
  if (last?.displayWidth !== frame.displayWidth || last.displayHeight !== frame.displayHeight) {
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
    post({ type: "frameSize", width: frame.displayWidth, height: frame.displayHeight });
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

/** A new decoder (`./opus.ts`: WebCodecs, or libopus in WebAssembly), and nothing stale in the
 *  worklet. */
function resetAudio() {
  audio?.close();
  audio = openOpus(
    onPcm,
    (e) => {
      console.warn("sound decoder error:", e);
      audioStream = -1; // the next packet starts again
    },
    AUDIO_FRAME_MS,
  );
  toWorklet({ t: "reset" });
}

function onAudio(buf: ArrayBuffer) {
  if (!soundOn || !soundPort) return;
  const h = parseAudioHeader(buf);
  if (h.stream !== audioStream || !audio) {
    resetAudio();
    audioStream = h.stream;
  }
  if (buf.byteLength === AUDIO_HEADER_LEN) return toWorklet({ t: "quiet" });
  audio!.decode(h.seq, new Uint8Array(buf, AUDIO_HEADER_LEN));
}

function onPcm(l: Float32Array, r: Float32Array) {
  // A packet still in the decoder when the sound went off.
  if (!soundOn) return;
  toWorklet({ t: "pcm", l, r }, [l.buffer, r.buffer]);
}

function setSound(on: boolean, port?: MessagePort) {
  if (port) soundPort = port;
  soundOn = on;
  audioStream = -1;
  toWorklet({ t: "reset" });
  send({ t: "sound", on });
}

/** Send an echo; something must be heard within `within` ms (a sooner limit stays). */
function probe(within: number) {
  if (ws?.readyState !== WebSocket.OPEN) return;
  const now = Date.now();
  echoAt = now;
  echoId = (echoId + 1) >>> 0;
  deadline = Math.min(deadline, now + within);
  ws.send(JSON.stringify({ t: "echo", id: echoId } satisfies FromBrowser));
}

/** Give up the socket at once, without waiting for its close (a half-open socket's close can
 *  take minutes), and open a new one now. The server replaces the old socket of this page with
 *  the new one (`tab`). */
function reopen() {
  const old = ws;
  if (old) {
    ws = null;
    const wasOpen = old.readyState === WebSocket.OPEN;
    old.onopen = old.onmessage = old.onclose = old.onerror = null;
    try {
      old.close();
    } catch {
      // closing a socket that never opened can throw in some browsers
    }
    if (wasOpen) post({ type: "connected", connected: false });
  }
  reconnectMs = 0;
  connect();
}

/** How long the socket now connecting may take. */
function connectLimit(): number {
  const base =
    connectTook === null ? CONNECT_MAX_MS : Math.min(Math.max(connectTook * 3, CONNECT_MIN_MS), CONNECT_MAX_MS);
  return timeouts % 2 ? Math.min(base * 2, CONNECT_MAX_MS) : base;
}

/** The page may have slept or been off the network (`wake` from the page: the network came
 *  back, the page was thawed or restored; or this worker's clock jumped: the machine slept).
 *  The socket from before it may be dead with no sign of it, and finding out costs seconds, so
 *  it is replaced at once: a new socket costs one keyframe. One waiting to retry tries now. */
function wake() {
  if (!ws) {
    if (retry !== undefined) {
      reconnectMs = 0;
      connect();
    }
    return;
  }
  if (Date.now() - startedAt < WAKE_AGAIN_MS) return;
  timeouts = 0;
  reopen();
}

let tickAt = 0;
/** Each LIVE_MS: an echo each ECHO_MS, a socket that went quiet or does not open replaced. */
function watch() {
  const now = Date.now();
  const late = tickAt ? now - tickAt - LIVE_MS : 0;
  // The clock was set back: the times kept are in the future, so start them again
  if (now < tickAt) {
    heardAt = startedAt = echoAt = now;
    deadline = Infinity;
  }
  tickAt = now;
  if (late > SLEPT_MS) wake();
  if (!ws) return;
  if (ws.readyState === WebSocket.CONNECTING) {
    if (now - startedAt > connectLimit()) {
      timeouts++;
      reopen();
    }
    return;
  }
  if (ws.readyState !== WebSocket.OPEN) return;
  if (now > deadline || (deadline !== Infinity && deadline - now > STALE_MS)) {
    console.warn(`viewer socket silent for ${now - heardAt} ms; reconnecting`);
    return reopen();
  }
  if (now - echoAt >= ECHO_MS) probe(STALE_MS);
}

function connect() {
  clearTimeout(retry);
  retry = undefined;
  startedAt = heardAt = Date.now();
  deadline = Infinity;
  echoAt = 0;
  clipIn = upAcked = 0;
  upOut = [];
  upRate = new RateMeter();
  upFloor = Infinity;
  newCopy();
  const sock = new WebSocket(url);
  ws = sock;
  sock.binaryType = "arraybuffer";
  sock.onopen = () => {
    heardAt = Date.now();
    connectTook = heardAt - startedAt;
    timeouts = 0;
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
  sock.onmessage = (e: MessageEvent<ArrayBuffer | string>) => {
    heardAt = Date.now();
    deadline = Infinity;
    if (typeof e.data === "string") {
      const msg = JSON.parse(e.data) as ToBrowser;
      if (msg.t === "echo") return;
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
  sock.onclose = () => {
    if (ws !== sock) return;
    ws = null;
    post({ type: "connected", connected: false });
    retry = setTimeout(connect, reconnectMs);
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
  if (m.type === "wake") {
    wake();
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
  // A GPU reset (it can come with a wake) clears the canvas: draw the last frame again
  m.canvas.addEventListener("contextrestored", draw);
  resetDecoder();
  connect();
  setInterval(() => {
    ack();
    checkHeld();
  }, ACK_MS);
  setInterval(watch, LIVE_MS);
};
