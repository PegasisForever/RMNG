// What the screen (`./FullDesk.tsx`) does under its layout: the connection to the selected
// clone (`./session.ts`), the live server state, and the input. In touch mode: the keys'
// touches, which type or move the pointer (`./keypad.ts`), a trackpad with no keys (the screen),
// the modifiers, Fn, Caps Lock, the key clicks and dictation. In mouse and keyboard mode
// (`./inputMode.ts`): the page's mouse, keys and clipboard (`~/viewer/input.ts`,
// `~/viewer/clipboard.ts`). And the layout claim. The screen only lays things out.
//
// A headless clone has a terminal in place of a desktop (`./TerminalScreen.tsx`, in monitor 0's
// window): the keys type into it (`./termKeys.ts`), Space's drag too (arrows), and dictation's
// words. The trackpads move a pointer the page draws over the terminal; its clicks, drags and
// scrolls go to the terminal as a mouse's events, so it selects, or the app that asked for the
// mouse gets them.
import { type RefObject, useCallback, useEffect, useRef, useState } from "react";

import { formatLxcUsage } from "~/lib/format";
import { type ClientKind, attachLayoutClaims, clientId, layoutClaim } from "~/lib/clientLayout";
import { useLiveState } from "~/lib/useLiveState";
import type { CursorMeta } from "~/lib/wire/CursorMeta";
import type { CursorShape } from "~/lib/wire/CursorShape";
import type { FromBrowser } from "~/lib/wire/FromBrowser";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { ScrollMode } from "~/lib/wire/ScrollMode";
import type { TermData } from "~/lib/wire/TermData";
import type { ViewSpec } from "~/lib/wire/ViewSpec";
import { attachClipboard } from "~/viewer/clipboard";
import { type Input, attachInput } from "~/viewer/input";
import { CODE_TO_EVDEV } from "~/viewer/keycodes";
import type { SoundReport } from "~/viewer/protocol";
import type { TermApi } from "~/viewer/terminal";

import { Dictation, type DictationState, textToKeysyms } from "./dictation";
import { BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, type TouchKind } from "./KeyboardView";
import { CAPS_LOCK, type Key, isShift } from "./keyboard";
import { Keypad, type KeypadOut } from "./keypad";
import { KeySounds } from "./keySound";
import { type ModState, Modifiers } from "./modifiers";
import { PHONE_SCROLL_SPEED, clampTo, pointerGain, scrollGain } from "./pointer";
import { type Session, openSession } from "./session";
import { isControlPanelChord, controlPanelChordHolds } from "./shortcut";
import { TermKeys } from "./termKeys";

/** The monitor size assumed until the server says the monitor's size. */
const START_MONITOR = { width: 1002, height: 808 };

/** After this page moved the cursor, the clone's own cursor reports are this far behind (ms),
 *  so they do not pull the drawn cursor back. */
const OWN_MOVES_MS = 400;
/** Mutter axis flags: a finger scroll, and its end. */
const AXIS_FINGER = 4;
const AXIS_FINISH = 1;

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
  const [mods, setMods] = useState<ReadonlyMap<number, ModState>>(new Map());
  /** The keys show as a trackpad (`./keypad.ts`: after a pointer gesture, for a while). */
  const [padMode, setPadMode] = useState(false);
  // The server's state, kept live: the tool row's numbers and everything the control panel shows
  const live = useLiveState();
  const [fn, setFn] = useState(false);
  const [caps, setCaps] = useState(false);
  /** How the clone's sound plays here, for the Usage page. */
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

  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;

  /** A headless clone: its terminal's output goes into `termSink`; `termApi` is its shown tab
   *  (`./TerminalScreen.tsx` sets both). */
  const terminal = spec?.content.mode === "terminal";
  const terminalRef = useRef(terminal);
  terminalRef.current = terminal;
  const termSink = useRef<((d: TermData) => void) | null>(null);
  const termApi = useRef<TermApi | null>(null);
  const termKeys = useRef(new TermKeys()).current;
  /** The terminal's pointer, CSS px in the terminal (drawn there), and a mouse's `buttons` down;
   *  null until a trackpad moves it. */
  const [termPos, setTermPos] = useState<{ x: number; y: number } | null>(null);
  const termPointer = useRef({ x: -1, y: -1, buttons: 0 });
  /** The terminal's pointer, put in its middle the first time, and kept inside it. */
  const termAt = (dx = 0, dy = 0) => {
    const p = termPointer.current;
    const size = termApi.current?.size();
    if (!size) return p;
    if (p.x < 0) [p.x, p.y] = [size.width / 2, size.height / 2];
    p.x = clampTo(p.x + dx, size.width);
    p.y = clampTo(p.y + dy, size.height);
    setTermPos({ x: p.x, y: p.y });
    return p;
  };
  const send = (msg: InputMsg) => {
    if (!terminalRef.current) return session.current?.send(msg);
    const api = termApi.current;
    if (msg.kind === "key_code") {
      const bytes = termKeys.key(msg.keycode, msg.pressed, api?.appCursor() ?? false);
      if (bytes) api?.input(bytes);
    } else if (msg.kind === "button") {
      const button = msg.button === BTN_RIGHT ? 2 : msg.button === BTN_MIDDLE ? 1 : 0;
      const bit = [1, 4, 2][button];
      const p = termAt();
      p.buttons = msg.pressed ? p.buttons | bit : p.buttons & ~bit;
      api?.mouse(msg.pressed ? "mousedown" : "mouseup", p.x, p.y, { button, buttons: p.buttons, ...termKeys.mods() });
    }
  };
  const sendTerm = useCallback((msg: FromBrowser) => session.current?.sendTerm(msg), []);

  /** Mouse mode's input and clipboard while they are attached, and what they are told when
   *  they attach: the last view spec, cursor and connection state. */
  const pageInput = useRef<{ input: Input; clip: ReturnType<typeof attachClipboard> } | null>(null);
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
        pageInput.current?.clip.setConnected(c);
      },
      onScroll(mode, speed) {
        scroll.current = { mode, speed };
        pageInput.current?.input.setScroll(mode, speed);
      },
      onClipboard: (msg) => pageInput.current?.clip.onMessage(msg),
      onClipboardData: (d) => pageInput.current?.clip.onData(d),
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

  // Mouse mode: the page's mouse and keys go to the clone
  const chordRef = useRef(onChord);
  chordRef.current = onChord;
  useEffect(() => {
    const s = session.current;
    if (!mouse || !s || !overlay.current) return;
    const clip = attachClipboard({ send: s.sendClipboard, sendData: s.sendClipboardData });
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
    const { spec: sp, cursor: c, connected: on } = last.current;
    if (sp) input.setLayout(sp.content.mode === "desktop" ? sp.monitors : []);
    if (c) input.onCursor(c);
    input.setScroll(scroll.current.mode, scroll.current.speed);
    clip.setConnected(on);
    pageInput.current = { input, clip };
    return () => {
      pageInput.current = null;
      input.detach();
      clip.detach();
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

  /** The modifiers, made once (`./modifiers.ts`): what they decide goes to the clone. */
  const modifiers = useRef<Modifiers | null>(null);
  if (!modifiers.current) {
    modifiers.current = new Modifiers(
      (code, pressed) => send({ kind: "key_code", keycode: code, pressed }),
      isShift,
    );
  }
  /** Run a change to the modifiers, then show their new states. */
  const withMods = (f: (m: Modifiers) => void) => {
    f(modifiers.current!);
    setMods(modifiers.current!.states());
  };
  /** A key that is no modifier goes down or up; a tapped modifier has done its key after it. */
  const keyCode = (code: number, pressed: boolean) => {
    if (pressed) {
      if (code === CAPS_LOCK) setCaps((c) => !c);
      modifiers.current!.keyDown();
    }
    send({ kind: "key_code", keycode: code, pressed });
    if (!pressed) withMods((m) => m.keyDone());
  };
  /** A mouse button, which counts as a key to the modifiers: Ctrl, then a click, is a
   *  Ctrl-click. */
  const button = (code: number, pressed: boolean) => {
    if (pressed) modifiers.current!.keyDown();
    send({ kind: "button", button: code, pressed });
    if (!pressed) withMods((m) => m.keyDone());
  };

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

  /** Dictation (`./dictation.ts`): its key's state, the words still being recognized (shown
   *  over the screen), and the text it is sure of, typed into the clone. */
  const [dictation, setDictation] = useState<DictationState>("off");
  const [heard, setHeard] = useState("");
  const dictate = useRef<Dictation | null>(null);
  if (!dictate.current) {
    dictate.current = new Dictation({
      state: setDictation,
      interim: setHeard,
      final: (text) => {
        if (terminalRef.current) return termApi.current?.input(text);
        for (const keysym of textToKeysyms(text)) {
          send({ kind: "key", keysym, pressed: true });
          send({ kind: "key", keysym, pressed: false });
        }
      },
    });
  }
  useEffect(() => {
    setDictation(dictate.current!.current);
    return () => dictate.current?.stop();
  }, []);

  /** Press the keys `codes` (DOM codes) in order, then let them go in the other order. */
  const chord = (codes: string[]) => {
    const keys = codes.map((c) => CODE_TO_EVDEV[c]);
    for (const keycode of keys) send({ kind: "key_code", keycode, pressed: true });
    for (const keycode of [...keys].reverse()) send({ kind: "key_code", keycode, pressed: false });
  };

  /** What both keypads do, the keys' and the screen's trackpad. */
  const out = (o: KeypadOut) => {
    switch (o.t) {
      case "press":
        sounds.play(o.key);
        break;
      case "tap":
        if (o.key.fn) setFn((f) => !f);
        else if (o.key.dictate) dictate.current!.toggle();
        else {
          keyCode(o.key.code, true);
          keyCode(o.key.code, false);
        }
        break;
      case "click": {
        // As from a mouse button: a tapped modifier makes it a Ctrl-click
        const code = o.button === "right" ? BTN_RIGHT : o.button === "middle" ? BTN_MIDDLE : BTN_LEFT;
        button(code, true);
        button(code, false);
        break;
      }
      case "drag":
        // Tap, then touch and move: the left button is down until the finger lifts
        button(BTN_LEFT, o.pressed);
        break;
      case "mod": {
        const { code } = o.key;
        const t = performance.now();
        withMods((m) => (o.down ? m.down(code, t) : m.up(code, o.dragged ?? false, t)));
        break;
      }
      case "hold":
        keyCode(o.key.code, o.pressed);
        break;
      case "move": {
        if (terminalRef.current) {
          const g = pointerGain(Math.hypot(o.dx, o.dy), o.ms);
          const p = termAt(o.dx * g, o.dy * g);
          termApi.current?.mouse("mousemove", p.x, p.y, { buttons: p.buttons, ...termKeys.mods() });
          break;
        }
        // Screen px for this finger motion, then monitor px
        const g = (pointerGain(Math.hypot(o.dx, o.dy), o.ms) * dpr) / scale.current;
        const c = cursor.current;
        const m = monitorRef.current;
        const x = clampTo(c.x + o.dx * g, m.width);
        const y = clampTo(c.y + o.dy * g, m.height);
        cursor.current = { x, y, movedAt: performance.now() };
        setPos({ x, y });
        send({ kind: "pointer_move", monitor_id: monitor, x, y });
        break;
      }
      case "scroll": {
        // The terminal's lines follow the fingers
        if (terminalRef.current) {
          const p = termAt();
          return termApi.current?.wheel(-o.dy, p.x, p.y);
        }
        const gain = scrollGain(scroll.current.speed);
        send({ kind: "axis_continuous", dx: o.dx * gain, dy: o.dy * gain, flags: AXIS_FINGER });
        break;
      }
      case "swipe":
        if (terminalRef.current) break;
        // Up or down: the Win key (GNOME's overview); left or right: Ctrl+Alt+arrow (the
        // workspace beside), the other way, as the content follows the fingers: a swipe to the
        // left brings in the workspace on the right. Chords of their own, past the page's
        // modifier keys.
        chord(
          o.dir === "up" || o.dir === "down"
            ? ["MetaLeft"]
            : ["ControlLeft", "AltLeft", o.dir === "left" ? "ArrowRight" : "ArrowLeft"],
        );
        break;
      case "scroll_end":
        send({ kind: "axis_continuous", dx: 0, dy: 0, flags: AXIS_FINGER | AXIS_FINISH });
        break;
      case "zoom":
        if (!terminalRef.current) zoomRef.current?.(o.factor);
        break;
      case "pad":
        break;
    }
  };
  const timer = {
    after(ms: number, f: () => void) {
      const id = setTimeout(f, ms);
      return () => clearTimeout(id);
    },
  };

  /** The keys' touches, made once: taps and holds type, the rest moves the pointer. */
  const keypad = useRef<Keypad | null>(null);
  if (!keypad.current) {
    keypad.current = new Keypad((o) => (o.t === "pad" ? setPadMode(o.on) : out(o)), timer);
  }
  /** A trackpad with no keys, for good (the screen while the keyboard floats), where two
   *  fingers also pinch to zoom the view. */
  const trackpad = useRef<Keypad | null>(null);
  if (!trackpad.current) trackpad.current = new Keypad(out, timer, { alwaysPad: true, pinch: true });

  const feed = (pad: Keypad, kind: TouchKind, id: number, key: Key | null, x: number, y: number) => {
    const t = performance.now();
    if (kind === "down") pad.down(id, key, x, y, t);
    else if (kind === "move") pad.move(id, x, y, t);
    else if (kind === "up") pad.up(id, t);
    else pad.cancel(id);
  };
  // Stable, so the keyboard does not render again each time the cursor moves
  const onKeyTouch = useCallback(
    (kind: TouchKind, id: number, key: Key | null, x: number, y: number) => feed(keypad.current!, kind, id, key, x, y),
    [],
  );
  const onPadTouch = useCallback(
    (kind: TouchKind, id: number, x: number, y: number) => feed(trackpad.current!, kind, id, null, x, y),
    [],
  );

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
          : null;

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
    mods,
    fn,
    caps,
    padMode,
    dictation,
    heard,
    soundReport,
    usage,
    unread,
    claim,
    sounds,
    onKeyTouch,
    onPadTouch,
  };
}
