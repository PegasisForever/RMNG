// The browser viewer: one window per monitor, `/viewer?monitor=<id>`, opened from the
// dashboard. The worker (`~/viewer/worker.ts`) does the socket, the decode and the drawing;
// this page holds the canvas, the stats overlay, and the one place server messages arrive.
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import type { ToBrowser } from "~/lib/wire/ToBrowser";
import type { ViewerStats, WorkerInit, WorkerOut } from "~/viewer/protocol";

export function meta() {
  return [{ title: "RMNG viewer" }];
}

/** Every server message other than video arrives here. Later rounds act on them. */
function onServerMessage(msg: ToBrowser) {
  switch (msg.t) {
    case "mode":
    case "view_spec":
    case "cursor":
    case "clipboard":
    case "term_data":
      break;
  }
}

export default function Viewer() {
  const [params] = useSearchParams();
  const monitor = Number(params.get("monitor") ?? 0);
  const box = useRef<HTMLDivElement>(null);
  // Null until the effect has looked: the page has no `window` while it is built.
  const [secure, setSecure] = useState<boolean | null>(null);
  const [connected, setConnected] = useState(false);
  const [stats, setStats] = useState<ViewerStats | null>(null);
  const [showStats, setShowStats] = useState(false);

  useEffect(() => {
    document.title = `RMNG monitor ${monitor}`;
    setSecure(window.isSecureContext);
  }, [monitor]);

  useEffect(() => {
    if (!secure || !box.current) return;
    // A fresh canvas per run: a canvas can hand its drawing to a worker only once.
    const canvas = document.createElement("canvas");
    canvas.className = "h-full w-full object-contain";
    box.current.append(canvas);
    const offscreen = canvas.transferControlToOffscreen();
    const worker = new Worker(new URL("../viewer/worker.ts", import.meta.url), {
      type: "module",
    });
    worker.onmessage = (e: MessageEvent<WorkerOut>) => {
      const m = e.data;
      if (m.type === "message") onServerMessage(m.msg);
      else if (m.type === "connected") setConnected(m.connected);
      else setStats(m.stats);
    };
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    const init: WorkerInit = {
      canvas: offscreen,
      url: `${scheme}//${location.host}/api/viewer?monitor=${monitor}`,
      monitor,
    };
    worker.postMessage(init, [offscreen]);
    return () => {
      worker.terminate();
      canvas.remove();
    };
  }, [secure, monitor]);

  if (secure === false) {
    return (
      <main className="fixed inset-0 flex items-center justify-center bg-black p-8 text-slate-200">
        <div className="max-w-lg space-y-3 text-sm">
          <h1 className="text-lg font-semibold text-amber-400">This page is not HTTPS</h1>
          <p>
            The viewer decodes video with WebCodecs, and Chrome allows WebCodecs only on HTTPS
            pages. Open the dashboard at its HTTPS address (for example through Tailscale HTTPS)
            and open the viewer from there.
          </p>
        </div>
      </main>
    );
  }

  return (
    <main className="fixed inset-0 bg-black">
      <div ref={box} className="h-full w-full" />
      {!connected ? (
        <p className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 text-sm text-slate-400">
          Connecting…
        </p>
      ) : null}
      {showStats && stats ? (
        <pre className="absolute left-2 top-2 rounded bg-black/70 px-2 py-1 font-mono text-xs text-green-300">
          {`fps     ${stats.fps}
decode  ${stats.decodeMs.toFixed(1)} ms
delay   ${stats.delayMs.toFixed(1)} ms
decoder ${stats.decoder}`}
        </pre>
      ) : null}
      <button
        type="button"
        onClick={() => setShowStats((s) => !s)}
        className="absolute right-2 top-2 rounded bg-black/50 px-2 py-0.5 text-xs text-slate-400 opacity-40 hover:opacity-100"
      >
        Stats
      </button>
    </main>
  );
}
