// The /ng page's connection to the selected clone: the browser viewer's socket for monitor 0,
// run by the viewer's own worker (`~/viewer/worker.ts`: socket, decode, drawing). The monitor
// is the iPhone Duo layout's, the same size in pixels as the box on the screen, so each
// monitor pixel is one screen pixel. Input does not come from the page's events here, as it
// does in the viewer: the on-screen keyboard and buttons send it (`send`).
import type { CursorMeta } from "~/lib/wire/CursorMeta";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { ToBrowser } from "~/lib/wire/ToBrowser";
import type { ViewSpec } from "~/lib/wire/ViewSpec";
import type { VideoStats, WorkerIn, WorkerOut } from "~/viewer/protocol";
import { type TrackView, showTrack } from "~/viewer/track";

export interface Session {
  send(msg: InputMsg): void;
  detach(): void;
}

export interface SessionEvents {
  onSpec(spec: ViewSpec): void;
  onCursor(c: CursorMeta): void;
  onConnected(connected: boolean): void;
  onStats(stats: VideoStats): void;
}

/** Where Safari can show the video: the worker draws each frame on the canvas, or writes it
 *  to a video track that a <video> shows (`~/viewer/worker.ts` has the trade-off). */
export type VideoMode = "canvas" | "track";

/** The viewer's choice: a track where the browser has the standard track generator (Safari),
 *  the canvas in Chrome. */
export const defaultVideoMode = (): VideoMode =>
  "MediaStreamTrackGenerator" in window ? "canvas" : "track";

/** The monitor shown, and the only one input goes to. */
export const MONITOR = 0;

/** Show monitor 0 of the selected clone in `box`, `width`×`height` device pixels at its top
 *  left, through `mode`. */
export function openSession(
  box: HTMLElement,
  { width, height }: { width: number; height: number },
  mode: VideoMode,
  events: SessionEvents,
): Session {
  const dpr = window.devicePixelRatio || 1;
  const canvas = document.createElement("canvas");
  canvas.className = "absolute left-0 top-0 block";
  canvas.style.width = `${width / dpr}px`;
  canvas.style.height = `${height / dpr}px`;
  box.append(canvas);
  const offscreen = canvas.transferControlToOffscreen();
  const worker = new Worker(new URL("../viewer/worker.ts", import.meta.url), { type: "module" });
  const post = (m: WorkerIn, transfer: Transferable[] = []) => worker.postMessage(m, transfer);
  let trackView: TrackView | null = null;

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
    }
  };
  worker.onmessage = (e: MessageEvent<WorkerOut>) => {
    const m = e.data;
    if (m.type === "message") onServerMessage(m.msg);
    else if (m.type === "track") {
      trackView?.detach();
      trackView = m.track ? showTrack(box, canvas, m.track) : null;
      trackView?.setBox(width, height);
    } else if (m.type === "connected") events.onConnected(m.connected);
    else if (m.type === "stats") events.onStats(m.stats);
  };
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  post(
    {
      type: "init",
      canvas: offscreen,
      url: `${scheme}//${location.host}/api/viewer?monitor=${MONITOR}`,
      monitor: MONITOR,
      track: mode === "track",
      stats: true,
    },
    [offscreen],
  );
  post({ type: "resize", width, height });

  return {
    send: (msg) => post({ type: "send", msg: { t: "input", ...msg } }),
    detach() {
      trackView?.detach();
      worker.terminate();
      canvas.remove();
    },
  };
}
