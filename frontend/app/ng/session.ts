// The /ng page's connection to the selected clone: the browser viewer's socket for one monitor
// of this browser's layout (`~/lib/clientLayout.ts`), run by the viewer's own worker
// (`~/viewer/worker.ts`: socket, decode, drawing, and, for monitor 0, the clone's sound, which
// `~/viewer/sound.ts` plays from the first touch). The page sizes the box it is shown in
// (`./geometry.ts`). In touch mode the input does not come from the page's events, as it does in
// the viewer: the on-screen keyboard and the trackpad send it (`send`). In mouse and keyboard
// mode the viewer's own input takes the page's events on `canvas` (`~/viewer/input.ts`), and the
// clipboard is shared (`~/viewer/clipboard.ts`).
import type { ClipboardMsg } from "~/lib/wire/ClipboardMsg";
import type { CursorMeta } from "~/lib/wire/CursorMeta";
import type { FromBrowser } from "~/lib/wire/FromBrowser";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { TouchMsg } from "~/lib/wire/TouchMsg";
import type { ToBrowser } from "~/lib/wire/ToBrowser";
import type { ScrollMode } from "~/lib/wire/ScrollMode";
import type { TermData } from "~/lib/wire/TermData";
import type { ViewSpec } from "~/lib/wire/ViewSpec";
import type { ClipboardBytes, SoundReport, WorkerIn, WorkerOut } from "~/viewer/protocol";
import { attachSound } from "~/viewer/sound";

export interface Session {
  /** The canvas the picture is drawn on (its placeholder: it takes the page's events). */
  canvas: HTMLCanvasElement;
  send(msg: InputMsg): void;
  sendClipboard(msg: ClipboardMsg): void;
  sendClipboardData(d: ClipboardBytes): void;
  /** A finger on the screen, as a touch screen's touch. */
  touch(msg: TouchMsg): void;
  /** This keyboard's Caps Lock, for the clone's to follow. */
  locks(caps: boolean): void;
  /** A message for a headless clone's terminal (`term_input`, `term_resize`,
   *  `term_new_session`). */
  sendTerm(msg: FromBrowser): void;
  /** The box is now `width`×`height` device pixels. */
  resize(width: number, height: number): void;
  detach(): void;
}

export interface SessionEvents {
  onSpec(spec: ViewSpec): void;
  onCursor(c: CursorMeta): void;
  onConnected(connected: boolean): void;
  /** The sound's report, once a second while it plays (`~/viewer/sound.ts`). */
  onSoundReport(r: SoundReport): void;
  /** The active layout's scroll mode and speed, percent (the server's `scroll` message). */
  onScroll(mode: ScrollMode, speed: number): void;
  /** The clone's clipboard: a message, and a reply's bytes. */
  onClipboard(msg: ClipboardMsg): void;
  onClipboardData(d: ClipboardBytes): void;
  /** A headless clone's terminal output (to monitor 0's window only). */
  onTermData(d: TermData): void;
}

/** The phone's sound buffer range, in 10 ms packets. */
const PHONE_SOUND_MIN = 4;
const PHONE_SOUND_MAX = 15;

/** The monitor whose window plays the clone's sound, so two windows never play it twice. */
const SOUND_MONITOR = 0;

/** Show `monitor` of the selected clone in `box`, which the canvas fills, scaled to fit its
 *  device pixels (`resize`). Always on the canvas: on an iPhone it ran better than the video
 *  track the viewer uses in Safari. */
export function openSession(box: HTMLElement, monitor: number, events: SessionEvents): Session {
  const canvas = document.createElement("canvas");
  canvas.className = "absolute inset-0 block h-full w-full";
  box.append(canvas);
  const offscreen = canvas.transferControlToOffscreen();
  const worker = new Worker(new URL("../viewer/worker.ts", import.meta.url), { type: "module" });
  const post = (m: WorkerIn, transfer: Transferable[] = []) => worker.postMessage(m, transfer);

  /** The build this page belongs to, from the first connection's `version` message. */
  let buildId: string | null = null;
  const onServerMessage = (msg: ToBrowser) => {
    switch (msg.t) {
      case "version":
        // The server came back on a different build: reload onto it, as the viewer does
        if (buildId !== null && msg.build_id !== buildId) location.reload();
        buildId = msg.build_id;
        break;
      case "view_spec":
        events.onSpec(msg);
        break;
      case "cursor":
        if (msg.monitor_id === monitor) events.onCursor(msg);
        break;
      case "scroll":
        events.onScroll(msg.mode, msg.speed);
        break;
      case "clipboard":
        events.onClipboard(msg);
        break;
      case "term_data":
        events.onTermData(msg);
        break;
    }
  };
  worker.onmessage = (e: MessageEvent<WorkerOut>) => {
    const m = e.data;
    if (m.type === "message") onServerMessage(m.msg);
    else if (m.type === "clipboard") events.onClipboardData(m.data);
    else if (m.type === "connected") events.onConnected(m.connected);
  };
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  post(
    {
      type: "init",
      canvas: offscreen,
      url: `${scheme}//${location.host}/api/viewer?monitor=${monitor}`,
      monitor,
      track: false,
    },
    [offscreen],
  );
  // Over a phone's Wi-Fi packets come less evenly than to a desktop, and the phone also decodes
  // the video: a deeper jitter buffer (40 to 150 ms) and iOS's longer output buffer, so the
  // sound does not crack.
  const sound =
    monitor === SOUND_MONITOR
      ? attachSound(post, {
          latencyHint: "playback",
          limits: { min: PHONE_SOUND_MIN, max: PHONE_SOUND_MAX },
          onReport: events.onSoundReport,
        })
      : null;

  return {
    canvas,
    send: (msg) => post({ type: "send", msg: { t: "input", ...msg } }),
    sendClipboard: (msg) => post({ type: "send", msg: { t: "clipboard", ...msg } }),
    sendClipboardData: (d) => post({ type: "clipboard", data: d }, [d.bytes.buffer]),
    touch: (msg) => post({ type: "send", msg: { t: "touch", ...msg } }),
    locks: (caps) => post({ type: "send", msg: { t: "locks", caps } }),
    sendTerm: (msg) => post({ type: "send", msg }),
    resize: (width, height) => post({ type: "resize", width, height }),
    detach() {
      sound?.detach();
      worker.terminate();
      canvas.remove();
    },
  };
}
