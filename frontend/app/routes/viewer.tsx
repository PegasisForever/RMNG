// The browser viewer: one window per monitor, `/viewer?monitor=<id>`, opened from the
// dashboard. The worker (`~/viewer/worker.ts`) does the socket, the decode and the drawing;
// this page holds the canvas, the input (`~/viewer/input.ts`), the clipboard
// (`~/viewer/clipboard.ts`), the cursor, the stats overlay (Ctrl+Alt+S) and file drop, and is
// where server messages arrive. For a clone with no desktop, monitor 0's window shows its
// terminal (`~/viewer/terminal.tsx`) over the canvas.
import { type DragEvent, useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import type { FromBrowser } from "~/lib/wire/FromBrowser";
import type { TermData } from "~/lib/wire/TermData";
import type { ToBrowser } from "~/lib/wire/ToBrowser";
import type { ViewContent } from "~/lib/wire/ViewContent";
import type { ViewMonitor } from "~/lib/wire/ViewMonitor";
import { attachClipboard } from "~/viewer/clipboard";
import { type Input, attachInput } from "~/viewer/input";
import {
  type ViewerStats,
  type WorkerIn,
  type WorkerOut,
  encodeClipboardData,
  parseClipboardData,
} from "~/viewer/protocol";
import { TermView } from "~/viewer/terminal";

export function meta() {
  return [{ title: "RMNG viewer" }];
}

/** Upload one dropped file into the selected clone's home; resolves to the name it got there. */
function uploadFile(file: File, onProgress: (fraction: number) => void): Promise<string> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest(); // fetch() reports no upload progress
    xhr.open("POST", `/api/viewer/upload?name=${encodeURIComponent(file.name)}`);
    xhr.upload.onprogress = (e) => onProgress(e.lengthComputable ? e.loaded / e.total : 0);
    xhr.onload = () =>
      xhr.status === 200
        ? resolve((JSON.parse(xhr.responseText) as { name: string }).name)
        : reject(new Error(xhr.responseText || `HTTP ${xhr.status}`));
    xhr.onerror = () => reject(new Error("network error"));
    xhr.send(file);
  });
}

export default function Viewer() {
  const [params] = useSearchParams();
  const monitor = Number(params.get("monitor") ?? 0);
  const box = useRef<HTMLDivElement>(null);
  const overlay = useRef<HTMLImageElement>(null);
  const input = useRef<Input | null>(null);
  /** The view spec's monitors; null until the first view spec. */
  const [layout, setLayout] = useState<ViewMonitor[] | null>(null);
  /** What the view spec says the windows show: video, or a terminal. */
  const [content, setContent] = useState<ViewContent | null>(null);
  /** Sends a message on the socket (through the worker); a no-op until the worker runs. */
  const sendRef = useRef<(msg: FromBrowser) => void>(() => {});
  const send = useCallback((msg: FromBrowser) => sendRef.current(msg), []);
  const termSink = useRef<((d: TermData) => void) | null>(null);
  // Null until the effect has looked: the page has no `window` while it is built.
  const [secure, setSecure] = useState<boolean | null>(null);
  const [connected, setConnected] = useState(false);
  const [stats, setStats] = useState<ViewerStats | null>(null);
  const [showStats, setShowStats] = useState(false);
  /** The file-drop note: progress, then the result for a few seconds. */
  const [note, setNote] = useState<{ text: string; error?: boolean } | null>(null);

  useEffect(() => {
    document.title = `RMNG monitor ${monitor}`;
    setSecure(window.isSecureContext);
  }, [monitor]);

  useEffect(() => {
    if (!secure || !box.current || !overlay.current) return;
    // A fresh canvas per run: a canvas can hand its drawing to a worker only once. The
    // placeholder stays in the page and takes the input.
    const canvas = document.createElement("canvas");
    canvas.className = "block h-full w-full";
    box.current.append(canvas);
    const offscreen = canvas.transferControlToOffscreen();
    const worker = new Worker(new URL("../viewer/worker.ts", import.meta.url), {
      type: "module",
    });
    const post = (m: WorkerIn, transfer: Transferable[] = []) => worker.postMessage(m, transfer);
    sendRef.current = (msg) => post({ type: "send", msg });
    const clip = attachClipboard({
      send: (msg) => post({ type: "send", msg: { t: "clipboard", ...msg } }),
      sendData: (d) => {
        const buf = encodeClipboardData(d);
        post({ type: "binary", buf }, [buf]);
      },
    });
    const inp = attachInput({
      canvas,
      overlay: overlay.current,
      monitor,
      send: (msg) => post({ type: "send", msg: { t: "input", ...msg } }),
      onToggleStats: () => setShowStats((v) => !v),
      onGesture: () => clip.onGesture(),
      onResize: (width, height) => post({ type: "resize", width, height }),
    });
    input.current = inp;

    // Every server message other than video arrives here.
    const onServerMessage = (msg: ToBrowser) => {
      switch (msg.t) {
        case "view_spec":
          // No pointer moves go to a clone that shows a terminal.
          inp.setLayout(msg.content.mode === "desktop" ? msg.monitors : []);
          setLayout(msg.monitors);
          setContent(msg.content);
          break;
        case "cursor":
          inp.onCursor(msg);
          break;
        case "clipboard":
          clip.onMessage(msg);
          break;
        case "term_data":
          termSink.current?.(msg);
          break;
      }
    };
    worker.onmessage = (e: MessageEvent<WorkerOut>) => {
      const m = e.data;
      if (m.type === "message") onServerMessage(m.msg);
      else if (m.type === "binary") clip.onData(parseClipboardData(m.buf));
      else if (m.type === "connected") {
        setConnected(m.connected);
        inp.setConnected(m.connected);
        clip.setConnected(m.connected);
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
      clip.detach();
      input.current = null;
      sendRef.current = () => {};
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

  // Files dropped on the window go to the selected clone's home, one after another.
  const onDragOver = (e: DragEvent) => {
    if (!e.dataTransfer.types.includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  };
  const onDrop = async (e: DragEvent) => {
    e.preventDefault();
    const done: string[] = [];
    for (const f of [...e.dataTransfer.files]) {
      try {
        const name = await uploadFile(f, (p) =>
          setNote({ text: `Uploading ${f.name} to ~/ … ${Math.round(p * 100)}%` }),
        );
        done.push(name);
      } catch (err) {
        setNote({ text: `Upload of ${f.name} failed: ${(err as Error).message}`, error: true });
        return;
      }
    }
    setNote({ text: `Uploaded to ~/: ${done.join(", ")}` });
    setTimeout(() => setNote(null), 5000);
  };

  return (
    <main className="fixed inset-0 select-none bg-black" onDragOver={onDragOver} onDrop={onDrop}>
      <div ref={box} className="h-full w-full" />
      <img
        ref={overlay}
        alt=""
        hidden
        className="pointer-events-none fixed left-0 top-0 max-w-none"
      />
      {/* Over the canvas, so the last video picture does not show through. A new clone remounts
          the terminal, so its tabs start empty. */}
      {content?.mode === "terminal" ? (
        monitor === 0 ? (
          <div className="absolute inset-0">
            <TermView key={content.clone} sessions={content.sessions} send={send} sink={termSink} />
          </div>
        ) : (
          <p className="absolute inset-0 flex items-center justify-center bg-black text-sm text-slate-400">
            This clone has no desktop. Its terminal is in the monitor 0 window.
          </p>
        )
      ) : layout && !layout.some((m) => m.id === monitor) ? (
        <p className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 text-sm text-slate-400">
          {layout.length
            ? `Monitor ${monitor} is not in the current layout. Close this window.`
            : "No clone is selected."}
        </p>
      ) : !connected ? (
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
      {note ? (
        <p
          className={`pointer-events-none absolute bottom-1 left-1 rounded bg-black/70 px-2 py-0.5 text-xs ${
            note.error ? "text-red-300" : "text-slate-200"
          }`}
        >
          {note.text}
        </p>
      ) : null}
    </main>
  );
}
