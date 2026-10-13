// The browser viewer: one window per monitor, `/viewer?monitor=<id>`, opened from the
// dashboard. The worker (`~/viewer/worker.ts`) does the socket, the decode and the drawing;
// this page holds the canvas, the input (`~/viewer/input.ts`), the cursor, the toolbar and
// the stats overlay, and is where server messages arrive.
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import type { ToBrowser } from "~/lib/wire/ToBrowser";
import { type Input, attachInput, toggleFullscreen } from "~/viewer/input";
import type { ViewerStats, WorkerIn, WorkerOut } from "~/viewer/protocol";

export function meta() {
  return [{ title: "RMNG viewer" }];
}

export default function Viewer() {
  const [params] = useSearchParams();
  const monitor = Number(params.get("monitor") ?? 0);
  const box = useRef<HTMLDivElement>(null);
  const overlay = useRef<HTMLImageElement>(null);
  const input = useRef<Input | null>(null);
  // Null until the effect has looked: the page has no `window` while it is built.
  const [secure, setSecure] = useState<boolean | null>(null);
  const [connected, setConnected] = useState(false);
  const [stats, setStats] = useState<ViewerStats | null>(null);
  const [showStats, setShowStats] = useState(false);
  const [locked, setLocked] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);

  useEffect(() => {
    document.title = `RMNG monitor ${monitor}`;
    setSecure(window.isSecureContext);
    const onFs = () => setFullscreen(document.fullscreenElement !== null);
    document.addEventListener("fullscreenchange", onFs);
    return () => document.removeEventListener("fullscreenchange", onFs);
  }, [monitor]);

  useEffect(() => {
    if (!secure || !box.current || !overlay.current) return;
    // A fresh canvas per run: a canvas can hand its drawing to a worker only once. The
    // placeholder stays in the page and takes the input.
    const canvas = document.createElement("canvas");
    canvas.className = "h-full w-full object-contain";
    box.current.append(canvas);
    const offscreen = canvas.transferControlToOffscreen();
    const worker = new Worker(new URL("../viewer/worker.ts", import.meta.url), {
      type: "module",
    });
    const post = (m: WorkerIn, transfer: Transferable[] = []) => worker.postMessage(m, transfer);
    const inp = attachInput({
      canvas,
      overlay: overlay.current,
      monitor,
      send: (msg) => post({ type: "send", msg: { t: "input", ...msg } }),
      onLockChange: setLocked,
    });
    input.current = inp;

    // Every server message other than video arrives here. Later rounds act on the rest.
    const onServerMessage = (msg: ToBrowser) => {
      switch (msg.t) {
        case "view_spec": {
          const m = msg.monitors.find((v) => v.id === monitor);
          if (m) inp.setMonitorSize(m.width, m.height);
          break;
        }
        case "cursor":
          inp.onCursor(msg);
          break;
        case "mode":
        case "clipboard":
        case "term_data":
          break;
      }
    };
    worker.onmessage = (e: MessageEvent<WorkerOut>) => {
      const m = e.data;
      if (m.type === "message") onServerMessage(m.msg);
      else if (m.type === "connected") {
        setConnected(m.connected);
        inp.setConnected(m.connected);
      } else setStats(m.stats);
    };
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    post(
      {
        type: "init",
        canvas: offscreen,
        url: `${scheme}//${location.host}/api/viewer?monitor=${monitor}`,
        monitor,
      },
      [offscreen],
    );
    return () => {
      inp.detach();
      input.current = null;
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

  // Toolbar buttons never take focus, so Space and Enter keep going to the clone.
  const tool = (label: string, on: boolean, act: () => void) => (
    <button
      type="button"
      tabIndex={-1}
      onMouseDown={(e) => e.preventDefault()}
      onClick={act}
      className={`rounded px-1.5 py-0.5 hover:bg-white/20 ${on ? "text-sky-300" : ""}`}
    >
      {label}
    </button>
  );

  return (
    <main className="fixed inset-0 select-none bg-black">
      <div ref={box} className="h-full w-full" />
      <img
        ref={overlay}
        alt=""
        hidden
        className="pointer-events-none fixed left-0 top-0 max-w-none"
      />
      {!connected ? (
        <p className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 text-sm text-slate-400">
          Connecting…
        </p>
      ) : null}
      {showStats && stats ? (
        <pre className="pointer-events-none absolute left-2 top-2 rounded bg-black/70 px-2 py-1 font-mono text-xs text-green-300">
          {`fps     ${stats.fps}
decode  ${stats.decodeMs.toFixed(1)} ms
delay   ${stats.delayMs.toFixed(1)} ms
decoder ${stats.decoder}`}
        </pre>
      ) : null}
      {/* Bottom right, faint: the clone's top bar has its own menus at the top. In fullscreen
          invisible until the pointer is on it. */}
      <div
        className={`absolute bottom-1 right-1 flex gap-0.5 rounded bg-black/60 text-xs text-slate-300 transition-opacity hover:opacity-100 ${
          fullscreen ? "opacity-0" : "opacity-40"
        }`}
      >
        {tool(fullscreen ? "Exit fullscreen" : "Fullscreen", fullscreen, toggleFullscreen)}
        {tool("Lock pointer", locked, () => input.current?.toggleLock())}
        {tool("Stats", showStats, () => setShowStats((s) => !s))}
      </div>
    </main>
  );
}
