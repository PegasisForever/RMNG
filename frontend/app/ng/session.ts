// The /ng page's connection to the selected clone: the browser viewer's socket for monitor 0,
// run by the viewer's own worker (`~/viewer/worker.ts`: socket, decode, drawing, and the
// clone's sound, which `~/viewer/sound.ts` plays from the first touch). The monitor is the
// phone's layout's (`./phoneMonitor.ts`); the page sizes the box it is shown in
// (`./geometry.ts`). Input does not come from the page's events here, as it does in the viewer:
// the on-screen keyboard and buttons send it (`send`).
import type { CursorMeta } from "~/lib/wire/CursorMeta";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { TouchMsg } from "~/lib/wire/TouchMsg";
import type { ToBrowser } from "~/lib/wire/ToBrowser";
import type { ViewSpec } from "~/lib/wire/ViewSpec";
import type { SoundReport, WorkerIn, WorkerOut } from "~/viewer/protocol";
import { attachSound } from "~/viewer/sound";

export interface Session {
  send(msg: InputMsg): void;
  /** A finger on the screen, as a touch screen's touch. */
  touch(msg: TouchMsg): void;
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
  /** The active layout's scroll speed, percent (the server's `scroll` message). */
  onScrollSpeed(speed: number): void;
}

/** The phone's sound buffer range, in 10 ms packets. */
const PHONE_SOUND_MIN = 4;
const PHONE_SOUND_MAX = 15;

/** The monitor shown, and the only one input goes to. */
export const MONITOR = 0;

/** Show monitor 0 of the selected clone in `box`, which the canvas fills, scaled to fit its
 *  device pixels (`resize`). Always on the canvas: on an iPhone it ran better than the video
 *  track the viewer uses in Safari. */
export function openSession(box: HTMLElement, events: SessionEvents): Session {
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
        if (msg.monitor_id === MONITOR) events.onCursor(msg);
        break;
      case "scroll":
        events.onScrollSpeed(msg.speed);
        break;
    }
  };
  worker.onmessage = (e: MessageEvent<WorkerOut>) => {
    const m = e.data;
    if (m.type === "message") onServerMessage(m.msg);
    else if (m.type === "connected") events.onConnected(m.connected);
  };
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  post(
    {
      type: "init",
      canvas: offscreen,
      url: `${scheme}//${location.host}/api/viewer?monitor=${MONITOR}`,
      monitor: MONITOR,
      track: false,
    },
    [offscreen],
  );
  // The only monitor, so the only window: it plays the clone's sound. Over the phone's Wi-Fi
  // packets come less evenly than to a desktop, and the phone also decodes the video: a deeper
  // jitter buffer (40 to 150 ms) and iOS's longer output buffer, so the sound does not crack.
  const sound = attachSound(post, {
    latencyHint: "playback",
    limits: { min: PHONE_SOUND_MIN, max: PHONE_SOUND_MAX },
    onReport: events.onSoundReport,
  });

  return {
    send: (msg) => post({ type: "send", msg: { t: "input", ...msg } }),
    touch: (msg) => post({ type: "send", msg: { t: "touch", ...msg } }),
    resize: (width, height) => post({ type: "resize", width, height }),
    detach() {
      sound.detach();
      worker.terminate();
      canvas.remove();
    },
  };
}
