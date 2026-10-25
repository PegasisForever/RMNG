// The /ng page on the iPhone Duo: the selected clone's screen on top, the keyboard below. This
// is the container: it holds the connection (`./session.ts`), the keyboard's state (held
// modifiers, Fn, Caps Lock) and the keys' touches, which type or move the pointer
// (`./keypad.ts`), and turns them into input.
import { type PointerEvent, useCallback, useEffect, useRef, useState } from "react";

import { formatLxcUsage } from "~/components/BoardRail";
import { layoutClaimer } from "~/lib/layoutClaim";
import { emptyState } from "~/lib/types";
import { useLiveState } from "~/lib/useLiveState";
import type { CursorShape } from "~/lib/wire/CursorShape";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { ViewSpec } from "~/lib/wire/ViewSpec";

import { Cursor } from "./Cursor";
import { BTN_LEFT, KeyboardView, type TouchKind } from "./KeyboardView";
import { keysLayout, screenFit } from "./geometry";
import { CAPS_LOCK, type Key, isShift } from "./keyboard";
import { Keypad, type KeypadOut } from "./keypad";
import { type ModState, Modifiers } from "./modifiers";
import { SCROLL_GAIN, clampTo, pointerGain } from "./pointer";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { MONITOR, type Session, openSession } from "./session";
import { isTyping, useLift } from "./systemKeyboard";
import type { NgTab } from "./ToolRow";

/** The iPhone Duo layout's one monitor, in pixels (`wire::iphone_duo_layout`). */
export const DUO_MONITOR = { width: 1002, height: 808 };
/** The keyboard area at the bottom of the screen, CSS px; the clone's screen has the rest
 *  (539 px on the Duo, the layout's monitor at 2×). */
const KEYS_HEIGHT = 412;
/** Touch slots a touch screen offers, fingers at once. */
const TOUCH_SLOTS = 10;

/** After this page moved the cursor, the clone's own cursor reports are this far behind (ms),
 *  so they do not pull the drawn cursor back. */
const OWN_MOVES_MS = 400;
/** Mutter axis flags: a finger scroll, and its end. */
const AXIS_FINGER = 4;
const AXIS_FINISH = 1;

const isDuoSpec = (s: ViewSpec) =>
  s.monitors.length === 1 &&
  s.monitors[0].width === DUO_MONITOR.width &&
  s.monitors[0].height === DUO_MONITOR.height;

type Props = {
  /** The whole screen, CSS px, and the home bar's safe-area inset. */
  width: number;
  height: number;
  bottom: number;
};

export function Desk({ width, height, bottom }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const session = useRef<Session | null>(null);
  const [spec, setSpec] = useState<ViewSpec | null>(null);
  const [connected, setConnected] = useState(false);
  const [shape, setShape] = useState<CursorShape | null>(null);
  const [hidden, setHidden] = useState(false);
  const [pos, setPos] = useState({ x: DUO_MONITOR.width / 2, y: DUO_MONITOR.height / 2 });
  const [mods, setMods] = useState<ReadonlyMap<number, ModState>>(new Map());
  /** The open tool-row tab; null shows the keyboard. */
  const [tab, setTab] = useState<NgTab | null>(null);
  /** The keys show as a trackpad (`./keypad.ts`: after a pointer gesture, for a while). */
  const [padMode, setPadMode] = useState(false);
  // The server's state, kept live: the tool row's numbers and everything the pages show
  const live = useLiveState(useRef(emptyState()).current);
  const [fn, setFn] = useState(false);
  const [caps, setCaps] = useState(false);
  /** The cursor where this page last put it, and when. */
  const cursor = useRef({ ...pos, movedAt: -Infinity });

  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  const fit = screenFit(width, height - KEYS_HEIGHT, DUO_MONITOR.width, DUO_MONITOR.height, dpr);
  /** Device px per monitor px now, for the cursor moves on the keys. */
  const scale = useRef(fit.scale);
  scale.current = fit.scale;

  const send = (msg: InputMsg) => session.current?.send(msg);

  useEffect(() => {
    if (!box.current) return;
    session.current = openSession(box.current, {
      onSpec: setSpec,
      onCursor(c) {
        if (c.hidden) setHidden(true);
        else if (c.shape) {
          setHidden(false);
          setShape(c.shape);
        }
        if (performance.now() - cursor.current.movedAt < OWN_MOVES_MS && !c.warp) return;
        cursor.current = { x: c.x, y: c.y, movedAt: cursor.current.movedAt };
        setPos({ x: c.x, y: c.y });
      },
      onConnected: setConnected,
    });
    return () => {
      session.current?.detach();
      session.current = null;
    };
  }, []);

  const usage = formatLxcUsage(live.lxcStats);
  /** Clones that stopped working and nobody has looked at yet: a count on the Clones tab. */
  const unread = live.state.hosts.filter((h) => h.unread && !h.archived && h.id !== live.state.selected).length;

  // The video's box in device pixels
  useEffect(() => {
    session.current?.resize(
      Math.round(DUO_MONITOR.width * fit.scale),
      Math.round(DUO_MONITOR.height * fit.scale),
    );
  }, [fit.scale]);

  // The phone takes the clones' layout when it opens, comes to the front, or is touched; a
  // click or a key in a desktop viewer window takes it back (`~/lib/layoutClaim.ts`)
  const claim = useRef(layoutClaimer("phone")).current;
  useEffect(() => {
    claim();
    const onVisible = () => document.visibilityState === "visible" && claim();
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [claim]);

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

  /** The keys' touches, made once: taps and holds type, the rest moves the pointer. */
  const keypad = useRef<Keypad | null>(null);
  if (!keypad.current) {
    const out = (o: KeypadOut) => {
      switch (o.t) {
        case "tap":
          if (o.key.fn) setFn((f) => !f);
          else {
            keyCode(o.key.code, true);
            keyCode(o.key.code, false);
          }
          break;
        case "pad":
          setPadMode(o.on);
          break;
        case "click":
          // A click, as from the Left button: a tapped modifier makes it a Ctrl-click
          modifiers.current!.keyDown();
          send({ kind: "button", button: BTN_LEFT, pressed: true });
          send({ kind: "button", button: BTN_LEFT, pressed: false });
          withMods((m) => m.keyDone());
          break;
        case "drag":
          // Tap, then touch and move: the left button is down until the finger lifts
          if (o.pressed) modifiers.current!.keyDown();
          send({ kind: "button", button: BTN_LEFT, pressed: o.pressed });
          if (!o.pressed) withMods((m) => m.keyDone());
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
          // Screen px for this finger motion, then monitor px
          const g = (pointerGain(Math.hypot(o.dx, o.dy), o.ms) * dpr) / scale.current;
          const c = cursor.current;
          const x = clampTo(c.x + o.dx * g, DUO_MONITOR.width);
          const y = clampTo(c.y + o.dy * g, DUO_MONITOR.height);
          cursor.current = { x, y, movedAt: performance.now() };
          setPos({ x, y });
          send({ kind: "pointer_move", monitor_id: MONITOR, x, y });
          break;
        }
        case "scroll":
          send({
            kind: "axis_continuous",
            dx: o.dx * SCROLL_GAIN,
            dy: o.dy * SCROLL_GAIN,
            flags: AXIS_FINGER,
          });
          break;
        case "scroll_end":
          send({ kind: "axis_continuous", dx: 0, dy: 0, flags: AXIS_FINGER | AXIS_FINISH });
          break;
      }
    };
    keypad.current = new Keypad(out, {
      after(ms, f) {
        const id = setTimeout(f, ms);
        return () => clearTimeout(id);
      },
    });
  }

  // Stable, so the keyboard does not render again each time the cursor moves
  const onKeyTouch = useCallback((kind: TouchKind, id: number, key: Key | null, x: number, y: number) => {
    const pad = keypad.current!;
    if (kind === "down") pad.down(id, key, x, y, performance.now());
    else if (kind === "move") pad.move(id, x, y, performance.now());
    else if (kind === "up") pad.up(id, performance.now());
    else pad.cancel(id);
  }, []);
  // A tab opens its page in the keyboard's place; tapped again, the keyboard comes back
  const onTab = useCallback((t: NgTab) => setTab((open) => (open === t ? null : t)), []);
  /** Where an open tab's page goes: the keys' and the mouse buttons' place. */
  const pageBox = keysLayout(width, KEYS_HEIGHT, bottom).page;
  // A click is a key to the modifiers: Ctrl, then a click, is a Ctrl-click
  const onButton = useCallback((button: number, pressed: boolean) => {
    if (pressed) modifiers.current!.keyDown();
    send({ kind: "button", button, pressed });
    if (!pressed) withMods((m) => m.keyDone());
  }, []);

  // While the phone's keyboard is up for a text box on a tab page, the whole screen moves up
  // so that page is all above the keyboard (its bottom edge is the home bar's top)
  const lift = useLift(height - bottom);
  // A touch outside the page puts the phone's keyboard away
  const onDownCapture = (e: PointerEvent<HTMLDivElement>) => {
    claim();
    if (isTyping() && !(e.target as Element).closest("[role=tabpanel]")) {
      (document.activeElement as HTMLElement).blur();
    }
  };

  const terminal = spec?.content.mode === "terminal";
  const note = !connected
    ? "Connecting…"
    : spec && spec.monitors.length === 0
      ? "No clone is selected."
      : terminal
        ? "This clone has no desktop."
        : spec && !isDuoSpec(spec)
          ? "In use on the desktop. Touch to use it here."
          : null;
  const place = { left: fit.left, top: fit.top, width: fit.width, height: fit.height };

  // Fingers on the screen go to the clone as a touch screen's touches, each in a slot of its
  // own from down to up
  const slots = useRef(new Map<number, number>());
  const touchAt = (e: PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    return {
      monitor_id: MONITOR,
      x: clampTo(((e.clientX - r.left) / r.width) * DUO_MONITOR.width, DUO_MONITOR.width),
      y: clampTo(((e.clientY - r.top) / r.height) * DUO_MONITOR.height, DUO_MONITOR.height),
    };
  };
  const onScreenDown = (e: PointerEvent<HTMLDivElement>) => {
    const used = new Set(slots.current.values());
    const slot = [...Array(TOUCH_SLOTS).keys()].find((s) => !used.has(s));
    if (slot === undefined) return;
    if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
    slots.current.set(e.pointerId, slot);
    session.current?.touch({ phase: "down", slot, ...touchAt(e) });
  };
  const onScreenMove = (e: PointerEvent<HTMLDivElement>) => {
    const slot = slots.current.get(e.pointerId);
    if (slot !== undefined) session.current?.touch({ phase: "motion", slot, ...touchAt(e) });
  };
  const onScreenUp = (e: PointerEvent<HTMLDivElement>) => {
    const slot = slots.current.get(e.pointerId);
    if (slot === undefined) return;
    slots.current.delete(e.pointerId);
    session.current?.touch({ phase: "up", slot });
  };

  return (
    <div
      className="absolute inset-0 bg-black transition-transform duration-300 ease-out"
      style={{ transform: `translateY(${-lift}px)` }}
      onPointerDownCapture={onDownCapture}
    >
      <div className="relative overflow-hidden" style={{ height: height - KEYS_HEIGHT }}>
        {/* The session puts the video in here, and touches on it go to the clone; the cursor
            and the note come after it, on top, and let touches through */}
        <div
          ref={box}
          className="absolute"
          style={place}
          onPointerDown={onScreenDown}
          onPointerMove={onScreenMove}
          onPointerUp={onScreenUp}
          onPointerCancel={onScreenUp}
        />
        <div className="pointer-events-none absolute" style={place}>
          {shape && !hidden && !terminal && connected ? (
            <Cursor shape={shape} x={pos.x} y={pos.y} scale={fit.scale} />
          ) : null}
        </div>
        {note ? (
          <p className="pointer-events-none absolute inset-0 flex items-center justify-center p-6">
            <span className="rounded-full bg-black/50 px-4 py-2 text-center text-sm text-neutral-100 backdrop-blur-md">
              {note}
            </span>
          </p>
        ) : null}
      </div>
      <KeyboardView
        width={width}
        height={KEYS_HEIGHT}
        bottom={bottom}
        fn={fn}
        mods={mods}
        caps={caps}
        onKeyTouch={onKeyTouch}
        onButton={onButton}
        usage={usage}
        tab={tab}
        onTab={onTab}
        badges={unread ? { clones: unread } : undefined}
        pad={padMode}
      />
      {/* Mounted from the start, so the pages' data is there when one opens */}
      <div
        className={`absolute ${tab ? "" : "hidden"}`}
        style={{ ...pageBox, top: height - KEYS_HEIGHT + pageBox.top }}
      >
        <NgPagesContainer tab={tab} onTab={setTab} state={live.state} stats={live.stats} />
      </div>
    </div>
  );
}
