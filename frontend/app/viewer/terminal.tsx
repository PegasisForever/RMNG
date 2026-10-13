// The terminal of a clone with no desktop: monitor 0's window shows it in place of the video.
// One xterm.js terminal per tmux session, as tabs, with a "+" that makes a new session, like the
// native viewer (`crates/viewer/src/terminal.rs`). The server (`control-server/src/termplane.rs`)
// sends each session's tmux output (`term_data`) and resizes every session to the one grid this
// view reports (`term_resize`). xterm.js does the keys, the mouse modes, selection and
// scrollback; Ctrl+Shift+C copies and Ctrl+Shift+V pastes.
import "@xterm/xterm/css/xterm.css";

import { FitAddon } from "@xterm/addon-fit";
import { type ITheme, Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";

import type { FromBrowser } from "~/lib/wire/FromBrowser";
import type { TermData } from "~/lib/wire/TermData";

/** The native viewer's dark theme (`viewer-core/src/terminal.rs`). */
const THEME: ITheme = {
  foreground: "#ffffff",
  background: "#1c1c1f",
  cursor: "#ffffff",
  selectionBackground: "#2b4766",
  black: "#26262b",
  red: "#c01c28",
  green: "#2ec27e",
  yellow: "#f5c211",
  blue: "#1e78e4",
  magenta: "#9841bb",
  cyan: "#0ab9dc",
  white: "#c0bfbc",
  brightBlack: "#5e5c64",
  brightRed: "#ed333b",
  brightGreen: "#57e389",
  brightYellow: "#f8e45c",
  brightBlue: "#51a1ff",
  brightMagenta: "#c061cb",
  brightCyan: "#4fd2fd",
  brightWhite: "#f6f5f4",
};

/** A window drag crosses many cell sizes, and each resize repaints every tmux session: send
 *  only the size the window settles on (the native viewer's debounce). */
const RESIZE_DEBOUNCE_MS = 90;

function toBase64(bytes: Uint8Array) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

interface Tab {
  term: Terminal;
  fit: FitAddon;
  el: HTMLDivElement;
}

export function TermView({
  sessions,
  send,
  sink,
}: {
  sessions: string[];
  send(msg: FromBrowser): void;
  /** The page puts each `term_data` it gets into this. */
  sink: { current: ((d: TermData) => void) | null };
}) {
  const box = useRef<HTMLDivElement>(null);
  const tabs = useRef(new Map<string, Tab>());
  const [active, setActive] = useState<string | null>(null);
  /** The grid last sent to the server. */
  const grid = useRef<{ cols: number; rows: number } | null>(null);
  const activeRef = useRef(active);
  activeRef.current = active;

  /** Fit the shown tab to the box, give every tab that grid (the server resizes every session
   *  to it), and tell the server when it changed. */
  const fit = () => {
    const shown = tabs.current.get(activeRef.current ?? "");
    const d = shown?.fit.proposeDimensions();
    if (!d || !d.cols || !d.rows || Number.isNaN(d.cols)) return;
    for (const t of tabs.current.values()) t.term.resize(d.cols, d.rows);
    if (grid.current?.cols === d.cols && grid.current?.rows === d.rows) return;
    grid.current = { cols: d.cols, rows: d.rows };
    send({ t: "term_resize", cols: d.cols, rows: d.rows });
  };

  // Output for a session goes to its tab, also while the tab is not shown.
  useEffect(() => {
    sink.current = (d) =>
      tabs.current.get(d.session)?.term.write(Uint8Array.from(atob(d.data), (c) => c.charCodeAt(0)));
    return () => {
      sink.current = null;
    };
  }, [sink]);

  // One tab per session: new sessions get a terminal, gone ones lose theirs.
  useEffect(() => {
    const had = tabs.current.size > 0;
    let added: string | null = null;
    for (const [name, t] of tabs.current) {
      if (sessions.includes(name)) continue;
      t.term.dispose();
      t.el.remove();
      tabs.current.delete(name);
    }
    for (const name of sessions) {
      if (tabs.current.has(name)) continue;
      const el = document.createElement("div");
      el.className = "absolute inset-0 invisible";
      box.current?.append(el);
      const term = new Terminal({
        theme: THEME,
        fontFamily: '"DejaVu Sans Mono", "Ubuntu Mono", monospace',
        fontSize: 14,
        lineHeight: 1.1,
        scrollback: 10000,
      });
      const f = new FitAddon();
      term.loadAddon(f);
      term.open(el);
      const g = grid.current;
      if (g) term.resize(g.cols, g.rows);
      const input = (data: string) =>
        send({ t: "term_input", session: name, data: toBase64(new TextEncoder().encode(data)) });
      term.onData(input);
      // Old-style mouse reports are bytes, not text.
      term.onBinary((data) => send({ t: "term_input", session: name, data: btoa(data) }));
      term.attachCustomKeyEventHandler((e) => {
        if (!(e.ctrlKey && e.shiftKey && (e.code === "KeyC" || e.code === "KeyV"))) return true;
        if (e.type === "keydown" && e.code === "KeyC") {
          e.preventDefault(); // Chrome's element inspector
          if (term.hasSelection()) void navigator.clipboard.writeText(term.getSelection());
        }
        // Ctrl+Shift+V: Chrome pastes into xterm's textarea, and xterm sends it (bracketed
        // when the app asked for that).
        return false;
      });
      tabs.current.set(name, { term, fit: f, el });
      added = name;
    }
    setActive((a) => {
      // A session that appeared later (the "+", or one made inside the clone) is shown; on the
      // first build the first tab is.
      if (had && added) return added;
      return a && tabs.current.has(a) ? a : (sessions[0] ?? null);
    });
  }, [sessions, send]);

  // Show the active tab only, fit it and give it the keys.
  useEffect(() => {
    for (const [name, t] of tabs.current) t.el.classList.toggle("invisible", name !== active);
    fit();
    if (active) tabs.current.get(active)?.term.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const ro = new ResizeObserver(() => {
      clearTimeout(timer);
      timer = setTimeout(fit, RESIZE_DEBOUNCE_MS);
    });
    if (box.current) ro.observe(box.current);
    const all = tabs.current;
    return () => {
      clearTimeout(timer);
      ro.disconnect();
      for (const t of all.values()) t.term.dispose();
      all.clear();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="flex h-full w-full flex-col bg-[#1c1c1f]">
      <div className="flex shrink-0 gap-0.5 bg-black px-1 pt-1 text-xs text-slate-300">
        {sessions.map((s) => (
          <button
            key={s}
            type="button"
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => setActive(s)}
            className={`rounded-t px-2 py-0.5 ${s === active ? "bg-[#1c1c1f] text-white" : "hover:bg-white/10"}`}
          >
            {s}
          </button>
        ))}
        <button
          type="button"
          title="New tmux session"
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => send({ t: "term_new_session" })}
          className="rounded-t px-2 py-0.5 hover:bg-white/10"
        >
          +
        </button>
      </div>
      {/* Padding like the native viewer's half-cell inset. */}
      <div className="relative min-h-0 flex-1 pl-1.5 pt-1">
        <div ref={box} className="relative h-full w-full" />
      </div>
    </div>
  );
}
