// The /ng page's connection to the selected clone: the browser viewer's socket for monitor 0,
// run by the viewer's own worker (`~/viewer/worker.ts`: socket, decode, drawing). The monitor
// is the iPhone Duo layout's, and the box on the screen is a whole multiple of it in device
// pixels, so each monitor pixel covers a square of whole screen pixels. Input does not come from the page's events here, as it
// does in the viewer: the on-screen keyboard and buttons send it (`send`).
import type { CursorMeta } from "~/lib/wire/CursorMeta";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { ToBrowser } from "~/lib/wire/ToBrowser";
import type { ViewSpec } from "~/lib/wire/ViewSpec";
import type { WorkerIn, WorkerOut } from "~/viewer/protocol";

export interface Session {
  send(msg: InputMsg): void;
  detach(): void;
}

export interface SessionEvents {
  onSpec(spec: ViewSpec): void;
  onCursor(c: CursorMeta): void;
  onConnected(connected: boolean): void;
}

/** The monitor shown, and the only one input goes to. */
export const MONITOR = 0;

/** Show monitor 0 of the selected clone in `box`, scaled to fit `width`×`height` device
 *  pixels at its top left. Always on the canvas: on the iPhone Duo it ran better than the
 *  video track the viewer uses in Safari. */
export function openSession(
  box: HTMLElement,
  { width, height }: { width: number; height: number },
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
  post({ type: "resize", width, height });

  return {
    send: (msg) => post({ type: "send", msg: { t: "input", ...msg } }),
    detach() {
      worker.terminate();
      canvas.remove();
    },
  };
}
