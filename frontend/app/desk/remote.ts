// What the screen (`./FullDesk.tsx`) does under its layout: the connection to the selected
// clone (`./session.ts`), the live server state, and the input. In touch mode: the keys' and
// the trackpad's touches (`./touchInput.ts`, which this shows and wires to the clone), and the
// key clicks. In mouse and keyboard mode (`./inputMode.ts`): the page's mouse and keys
// (`~/viewer/input.ts`). In both the clipboard (`~/viewer/clipboard.ts`). And the layout
// claim. The screen only lays things out.
//
// A headless clone has a terminal in place of a desktop (`./TerminalScreen.tsx`, in monitor 0's
// window): the touch input goes to it (`./cloneSink.ts`), and the trackpads move a pointer the
// page draws over it.
import { type RefObject, useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";

import { formatLxcUsage } from "~/lib/format";
import { type ClientKind, attachLayoutClaims, clientId, layoutClaim } from "~/lib/clientLayout";
import { useLiveState } from "~/lib/useLiveState";
import type { CursorMeta } from "~/lib/wire/CursorMeta";
import type { CursorShape } from "~/lib/wire/CursorShape";
import type { FromBrowser } from "~/lib/wire/FromBrowser";
import type { ScrollMode } from "~/lib/wire/ScrollMode";
import type { TermData } from "~/lib/wire/TermData";
import type { ViewSpec } from "~/lib/wire/ViewSpec";
import { attachClipboard } from "~/viewer/clipboard";
import { type Input, attachInput } from "~/viewer/input";
import type { SoundReport } from "~/viewer/protocol";
import type { TermApi } from "~/viewer/terminal";

import { desktopSink, terminalSink } from "./cloneSink";
import { KeySounds } from "./keySound";
import { PHONE_SCROLL_SPEED } from "./pointer";
import { type Session, openSession } from "./session";
import { isControlPanelChord, controlPanelChordHolds } from "./shortcut";
import { type Clock, TouchInput } from "./touchInput";

/** The monitor size assumed until the server says the monitor's size. */
const START_MONITOR = { width: 1002, height: 808 };

/** After this page moved the cursor, the clone's own cursor reports are this far behind (ms),
 *  so they do not pull the drawn cursor back. */
const OWN_MOVES_MS = 400;

/** The page's own time and waits, for the touch input. */
const PAGE_CLOCK: Clock = {
  now: () => performance.now(),
  after(ms, f) {
    const id = setTimeout(f, ms);
    return () => clearTimeout(id);
  },
};

type Size = { width: number; height: number };

export function useRemote({
  box,
  overlay,
  monitor,
  kind,
  mouse,
  onZoom,
  onChord,
}: {
  /** Where the picture goes. */
  box: RefObject<HTMLDivElement | null>;
  /** The drawn cursor of mouse mode, where the CSS cursor cannot show it (`~/viewer/input.ts`). */
  overlay: RefObject<HTMLImageElement | null>;
  /** The monitor of this browser's layout this window shows. */
  monitor: number;
  /** What this browser claims as (`~/lib/clientLayout.ts`). */
  kind: ClientKind;
  /** Mouse and keyboard mode, with nothing over the screen: the page's mouse and keys go to the
   *  clone. */
  mouse: boolean;
  /** A pinch on the trackpad (the screen's): the view's zoom changes by `factor`. */
  onZoom?: (factor: number) => void;
  /** The control panel's chord was pressed (`./shortcut.ts`), in mouse mode. */
  onChord: () => void;
}) {
  const session = useRef<Session | null>(null);
  const [spec, setSpec] = useState<ViewSpec | null>(null);
  const [connected, setConnected] = useState(false);
  const [shape, setShape] = useState<CursorShape | null>(null);
  const [hidden, setHidden] = useState(false);
  const [pos, setPos] = useState({ x: START_MONITOR.width / 2, y: START_MONITOR.height / 2 });
  // The server's state, kept live: the tool row's numbers and everything the control panel shows
  const live = useLiveState();
  /** How the clone's sound plays here, for the Usage tab. */
  const [soundReport, setSoundReport] = useState<SoundReport | null>(null);
  /** The layout's scroll mode and speed, percent: the server says them when the socket opens. */
  const scroll = useRef<{ mode: ScrollMode; speed: number }>({ mode: "auto", speed: PHONE_SCROLL_SPEED });
  /** The cursor where this page last put it, and when. */
  const cursor = useRef({ ...pos, movedAt: -Infinity });

  /** The clone's monitor shown (START_MONITOR until the server says). */
  const shown = spec?.monitors.find((m) => m.id === monitor);
  const screen: Size = shown ? { width: shown.width, height: shown.height } : START_MONITOR;
  const monitorRef = useRef(screen);
  monitorRef.current = screen;
  /** Device px per monitor px now (the screen sets it), for the cursor moves. */
  const scale = useRef(1);

  /** A headless clone: its terminal's output goes into `termSink`; `termApi` is its shown tab
   *  (`./TerminalScreen.tsx` sets both). */
  const terminal = spec?.content.mode === "terminal";
  const terminalRef = useRef(terminal);
  terminalRef.current = terminal;
  const termSink = useRef<((d: TermData) => void) | null>(null);
  const termApi = useRef<TermApi | null>(null);
  /** The terminal's pointer, CSS px in the terminal (drawn there); null until a trackpad moves
   *  it. */
  const [termPos, setTermPos] = useState<{ x: number; y: number } | null>(null);
  const sendTerm = useCallback((msg: FromBrowser) => session.current?.sendTerm(msg), []);

  /** The clipboard of the session, and mouse mode's input while it is attached; and what they
   *  are told when they attach: the last view spec, cursor and connection state. */
  const clipboard = useRef<ReturnType<typeof attachClipboard> | null>(null);
  const pageInput = useRef<{ input: Input } | null>(null);
  const last = useRef<{ spec: ViewSpec | null; cursor: CursorMeta | null; connected: boolean }>({
    spec: null,
    cursor: null,
    connected: false,
  });
  /** A new session each time the monitor changes: mouse mode attaches to its canvas. */
  const [sessionNo, setSessionNo] = useState(0);

  useEffect(() => {
    if (!box.current) return;
    session.current = openSession(box.current, monitor, {
      onSpec(s) {
        setSpec(s);
        last.current.spec = s;
        pageInput.current?.input.setLayout(s.content.mode === "desktop" ? s.monitors : []);
      },
      onCursor(c) {
        last.current.cursor = c;
        pageInput.current?.input.onCursor(c);
        if (c.hidden) setHidden(true);
        else if (c.shape) {
          setHidden(false);
          setShape(c.shape);
        }
        if (performance.now() - cursor.current.movedAt < OWN_MOVES_MS && !c.warp) return;
        cursor.current = { x: c.x, y: c.y, movedAt: cursor.current.movedAt };
        setPos({ x: c.x, y: c.y });
      },
      onConnected(c) {
        setConnected(c);
        last.current.connected = c;
        clipboard.current?.setConnected(c);
      },
      onScroll(mode, speed) {
        scroll.current = { mode, speed };
        pageInput.current?.input.setScroll(mode, speed);
      },
      onClipboard: (msg) => clipboard.current?.onMessage(msg),
      onClipboardData: (d) => clipboard.current?.onData(d),
      onTermData: (d) => termSink.current?.(d),
      onSoundReport: setSoundReport,
    });
    setSessionNo((n) => n + 1);
    return () => {
      session.current?.detach();
      session.current = null;
      last.current = { spec: null, cursor: null, connected: false };
    };
  }, [box, monitor]);

  // The clipboard, in both modes: the clipboard keys use it in touch mode
  useEffect(() => {
    const s = session.current;
    if (!s) return;
    const clip = attachClipboard({ send: s.sendClipboard, sendData: s.sendClipboardData });
    clip.setConnected(last.current.connected);
    clipboard.current = clip;
    return () => {
      clipboard.current = null;
      clip.detach();
    };
  }, [sessionNo]);

  // Mouse mode: the page's mouse and keys go to the clone
  const chordRef = useRef(onChord);
  chordRef.current = onChord;
  useEffect(() => {
    const s = session.current;
    const clip = clipboard.current;
    if (!mouse || !s || !clip || !overlay.current) return;
    const input = attachInput({
      canvas: s.canvas,
      overlay: overlay.current,
      monitor,
      send: s.send,
      sendLocks: s.locks,
      onGesture: () => clip.onGesture(),
      pasteByEvent: () => clip.pasteByEvent(),
      chord: { is: (e) => isControlPanelChord(e), holds: controlPanelChordHolds(), run: () => chordRef.current() },
    });
    const { spec: sp, cursor: c } = last.current;
    if (sp) input.setLayout(sp.content.mode === "desktop" ? sp.monitors : []);
    if (c) input.onCursor(c);
    input.setScroll(scroll.current.mode, scroll.current.speed);
    pageInput.current = { input };
    return () => {
      pageInput.current = null;
      input.detach();
    };
  }, [mouse, monitor, sessionNo, overlay]);

  const usage = formatLxcUsage(live.lxcStats);
  /** Clones that stopped working and nobody has looked at yet: a count on the Clones tab. */
  const unread = live.state.hosts.filter((h) => h.unread && !h.archived && h.id !== live.state.selected).length;

  // This browser's layout is in force when the page opens, comes to the front, or is used,
  // while it is in the foreground; an action in another client takes the clones
  // (`~/lib/clientLayout.ts`)
  const zoomRef = useRef(onZoom);
  zoomRef.current = onZoom;
  const claims = useRef<ReturnType<typeof attachLayoutClaims> | null>(null);
  useEffect(() => {
    claims.current = attachLayoutClaims(kind, layoutClaim);
    return () => claims.current?.detach();
  }, [kind]);
  /** Put the stored layout in force now: it changed. */
  const claim = useCallback(() => claims.current?.claim(true), []);

  // The clone's sound plays only where the clones are used: in the foreground, in the browser
  // whose layout is in force (a client that takes the clones takes their sound). Anywhere else
  // the page's audio is closed, the key clicks' too in the background, so a phone does not show
  // the page as playing. The server sends the sound to the window that asked for it last.
  const [front, setFront] = useState(() => typeof document === "undefined" || document.visibilityState === "visible");
  useEffect(() => {
    const look = () => setFront(document.visibilityState === "visible");
    const gone = () => setFront(false);
    document.addEventListener("visibilitychange", look);
    window.addEventListener("pageshow", look);
    window.addEventListener("pagehide", gone);
    return () => {
      document.removeEventListener("visibilitychange", look);
      window.removeEventListener("pageshow", look);
      window.removeEventListener("pagehide", gone);
    };
  }, []);
  const inUse = !live.state.layout || live.state.layout.client === clientId(kind);
  const soundHere = front && inUse;
  useEffect(() => {
    session.current?.setSound(soundHere);
    if (!soundHere) setSoundReport(null);
  }, [soundHere, sessionNo]);

  /** The keys' click sound (`./keySound.ts`). iOS starts audio only inside a touch, and a lift
   *  is what counts as one there, so each lift tries. */
  const sounds = useRef(new KeySounds()).current;
  useEffect(() => {
    const unlock = () => sounds.unlock();
    document.addEventListener("touchend", unlock);
    document.addEventListener("pointerup", unlock);
    return () => {
      document.removeEventListener("touchend", unlock);
      document.removeEventListener("pointerup", unlock);
    };
  }, [sounds]);
  // In the background the clicks' audio is closed too (see the clone's sound above)
  useEffect(() => {
    if (!front) sounds.close();
  }, [front, sounds]);

  /** The touch input, made once (`./touchInput.ts`): it goes to the desktop through the
   *  session, or to the terminal of a headless clone (`./cloneSink.ts`). */
  const monitorId = useRef(monitor);
  monitorId.current = monitor;
  const touchRef = useRef<TouchInput | null>(null);
  if (!touchRef.current) {
    const desktop = desktopSink(
      () => session.current,
      () => ({
        monitor: monitorId.current,
        ...monitorRef.current,
        px: (window.devicePixelRatio || 1) / scale.current,
        scrollSpeed: scroll.current.speed,
        cursor: cursor.current,
        moved(x, y) {
          cursor.current = { x, y, movedAt: performance.now() };
          setPos({ x, y });
        },
      }),
    );
    const term = terminalSink(() => termApi.current, setTermPos);
    touchRef.current = new TouchInput({
      sink: () => (terminalRef.current ? term : desktop),
      clock: PAGE_CLOCK,
      clipboard: () => clipboard.current,
      press: (key) => sounds.play(key),
      zoom: (factor) => zoomRef.current?.(factor),
    });
  }
  const touch = touchRef.current;
  useEffect(() => () => touch.stop(), [touch]);
  /** What the keys show (modifiers, Fn, Caps Lock, trackpad mode, dictation) and the clipboard
   *  keys' note. */
  const keys = useSyncExternalStore(touch.subscribe, touch.state, touch.state);

  const note = !connected
    ? "Connecting…"
    : spec && spec.monitors.length === 0
      ? "No clone is selected."
      : terminal
        ? monitor === 0
          ? null
          : "This clone has no desktop. Its terminal is in the monitor 0 window."
        : live.state.layout && live.state.layout.client !== clientId(kind)
          ? `In use on ${live.state.layout.label === kind ? "another" : "the"} ${live.state.layout.label}. ${
              mouse ? "Click" : "Touch"
            } to use it here.`
          : keys.clipNote;

  return {
    session,
    live,
    spec,
    monitor: screen,
    connected,
    terminal,
    /** The terminal's sessions, and the clone they are of; null: no headless clone. */
    termContent: spec?.content.mode === "terminal" ? spec.content : null,
    termSink,
    termApi,
    termPos,
    sendTerm,
    note,
    shape,
    hidden,
    pos,
    scale,
    mods: keys.mods,
    fn: keys.fn,
    caps: keys.caps,
    padMode: keys.pad,
    dictation: keys.dictation,
    heard: keys.heard,
    soundReport,
    usage,
    unread,
    claim,
    sounds,
    // Stable, so the keyboard does not render again each time the cursor moves
    onKeyTouch: touch.keyTouch,
    onPadTouch: touch.padTouch,
  };
}
