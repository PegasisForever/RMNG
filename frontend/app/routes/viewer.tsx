// The browser viewer: one window per monitor, `/viewer?monitor=<id>`, opened from the
// dashboard. The worker (`~/viewer/worker.ts`) does the socket, the decode and the drawing
// (in Safari, into a track that this page shows in a <video>: `~/viewer/track.ts`);
// this page holds the canvas, the input (`~/viewer/input.ts`), the clipboard
// (`~/viewer/clipboard.ts`), the cursor and file drop, and is
// where server messages arrive. For a clone with no desktop, monitor 0's window shows its
// terminal (`~/viewer/terminal.tsx`) over the canvas. Monitor 0's window also plays the selected
// clone's sound (`~/viewer/sound.ts`), from the first click or key in it.
import { type DragEvent, useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import { layoutClaimer } from "~/lib/layoutClaim";
import type { FromBrowser } from "~/lib/wire/FromBrowser";
import type { TermData } from "~/lib/wire/TermData";
import type { ToBrowser } from "~/lib/wire/ToBrowser";
import type { ViewContent } from "~/lib/wire/ViewContent";
import type { ViewMonitor } from "~/lib/wire/ViewMonitor";
import { attachClipboard } from "~/viewer/clipboard";
import { type Input, attachInput, toggleFullscreen } from "~/viewer/input";
import type { WorkerIn, WorkerOut } from "~/viewer/protocol";
import { attachSound } from "~/viewer/sound";
import { TermView } from "~/viewer/terminal";
import { type TrackView, showTrack } from "~/viewer/track";

export function meta() {
  return [{ title: "RMNG viewer" }];
}

/** Only monitor 0's window plays the sound, so two windows never play it twice. */
const SOUND_MONITOR = 0;

/** Whether the worker shows the video through a track (`~/viewer/track.ts`) where it can.
 *  Chrome keeps the canvas, which works well there; the track generator Chrome has instead is
 *  its own one, `MediaStreamTrackGenerator`, which tells Chrome apart. `?sink=canvas` or
 *  `?sink=track` chooses, to compare the two. */
function wantTrack(sink: string | null): boolean {
  return sink ? sink === "track" : !("MediaStreamTrackGenerator" in window);
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
  const sink = params.get("sink");
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
  /** The file-drop note: progress, then the result for a few seconds. */
  const [note, setNote] = useState<{ text: string; error?: boolean } | null>(null);
  const [fullscreen, setFullscreen] = useState(false);

  useEffect(() => {
    document.title = `RMNG monitor ${monitor}`;
    setSecure(window.isSecureContext);
    const onFs = () => setFullscreen(document.fullscreenElement !== null);
    document.addEventListener("fullscreenchange", onFs);
    return () => document.removeEventListener("fullscreenchange", onFs);
  }, [monitor]);

  // A click or a key here takes the clones' layout back from the phone, if it has it
  useEffect(() => {
    const claim = layoutClaimer("desktop");
    window.addEventListener("pointerdown", claim, true);
    window.addEventListener("keydown", claim, true);
    return () => {
      window.removeEventListener("pointerdown", claim, true);
      window.removeEventListener("keydown", claim, true);
    };
  }, []);

  useEffect(() => {
    if (!secure || !box.current || !overlay.current) return;
    // A fresh canvas per run: a canvas can hand its drawing to a worker only once. The
    // placeholder stays in the page and takes the input.
    const canvas = document.createElement("canvas");
    // Over the <video> of a track, which the page puts in front of it in the box.
    canvas.className = "relative block h-full w-full";
    box.current.append(canvas);
    const offscreen = canvas.transferControlToOffscreen();
    const worker = new Worker(new URL("../viewer/worker.ts", import.meta.url), {
      type: "module",
    });
    const post = (m: WorkerIn, transfer: Transferable[] = []) => worker.postMessage(m, transfer);
    let trackView: TrackView | null = null;
    /** The canvas's size in device pixels, for a track view that starts later. */
    let size: [number, number] | null = null;
    sendRef.current = (msg) => post({ type: "send", msg });
    const clip = attachClipboard({
      send: (msg) => post({ type: "send", msg: { t: "clipboard", ...msg } }),
      sendData: (d) => post({ type: "clipboard", data: d }, [d.bytes.buffer]),
    });
    const inp = attachInput({
      canvas,
      overlay: overlay.current,
      monitor,
      send: (msg) => post({ type: "send", msg: { t: "input", ...msg } }),
      onGesture: () => clip.onGesture(),
      pasteByEvent: () => clip.pasteByEvent(),
      onResize: (width, height) => {
        size = [width, height];
        trackView?.setBox(width, height);
        post({ type: "resize", width, height });
      },
    });
    input.current = inp;
    // The worker decodes the sound with WebCodecs, or with libopus in WebAssembly where the
    // browser's WebCodecs takes no Opus (`~/viewer/opus.ts`)
    const snd = monitor === SOUND_MONITOR ? attachSound(post) : null;

    /** The build this page belongs to, from the first connection's `version` message. */
    let buildId: string | null = null;
    // Every server message other than video arrives here.
    const onServerMessage = (msg: ToBrowser) => {
      switch (msg.t) {
        case "version":
          // The server came back on a different build: this page's code may no longer match
          // it, so reload, as the dashboard does.
          if (buildId !== null && msg.build_id !== buildId) location.reload();
          buildId = msg.build_id;
          break;
        case "scroll":
          inp.setScroll(msg.mode, msg.speed);
          break;
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
      else if (m.type === "clipboard") clip.onData(m.data);
      else if (m.type === "track") {
        trackView?.detach();
        trackView = m.track ? showTrack(box.current!, canvas, m.track) : null;
        if (size) trackView?.setBox(...size);
      } else {
        setConnected(m.connected);
        clip.setConnected(m.connected);
      }
    };
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    post(
      {
        type: "init",
        canvas: offscreen,
        url: `${scheme}//${location.host}/api/viewer?monitor=${monitor}`,
        monitor,
        track: wantTrack(sink),
      },
      [offscreen],
    );
    return () => {
      trackView?.detach();
      snd?.detach();
      inp.detach();
      clip.detach();
      input.current = null;
      sendRef.current = () => {};
      worker.terminate();
      canvas.remove();
    };
  }, [secure, monitor, sink]);

  if (secure === false) {
    return (
      <main className="fixed inset-0 flex items-center justify-center bg-black p-8 text-slate-200">
        <div className="max-w-lg space-y-3 text-sm">
          <h1 className="text-lg font-semibold text-amber-400">This page is not HTTPS</h1>
          <p>
            The viewer decodes video with WebCodecs, and browsers allow WebCodecs only on HTTPS
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
      <div ref={box} className="relative h-full w-full" />
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
      {note ? (
        <p
          className={`pointer-events-none absolute bottom-1 left-1 rounded bg-black/70 px-2 py-0.5 text-xs ${
            note.error ? "text-red-300" : "text-slate-200"
          }`}
        >
          {note.text}
        </p>
      ) : null}
      {/* Only outside fullscreen: the operator leaves fullscreen with the window's own controls.
          It never takes focus, so keys keep going to the clone. */}
      {fullscreen ? null : (
        <button
          type="button"
          tabIndex={-1}
          onMouseDown={(e) => e.preventDefault()}
          onClick={toggleFullscreen}
          className="absolute left-1 top-1 rounded bg-black/60 px-2 py-0.5 text-xs text-slate-200"
        >
          Fullscreen
        </button>
      )}
    </main>
  );
}
