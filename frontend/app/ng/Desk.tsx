// The /ng page on the iPhone Duo: the selected clone's screen on top, the keyboard below. This
// is the container: it holds the connection (`./session.ts`), the keyboard's state (held
// modifiers, Fn, Caps Lock) and the keys' touches, which type or move the pointer
// (`./keypad.ts`), and turns them into input.
import { useCallback, useEffect, useRef, useState } from "react";

import { layoutClaimer } from "~/lib/layoutClaim";
import type { CursorShape } from "~/lib/wire/CursorShape";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { ViewSpec } from "~/lib/wire/ViewSpec";

import { Cursor } from "./Cursor";
import { KeyboardView, type TouchKind } from "./KeyboardView";
import { screenFit } from "./geometry";
import { CAPS_LOCK, type Key, isShift } from "./keyboard";
import { Keypad, type KeypadOut } from "./keypad";
import { type ModState, Modifiers } from "./modifiers";
import { SCROLL_GAIN, clampTo, pointerGain } from "./pointer";
import { MONITOR, type Session, openSession } from "./session";

/** The iPhone Duo layout's one monitor, in pixels (`wire::iphone_duo_layout`). */
export const DUO_MONITOR = { width: 1002, height: 760 };
/** The keyboard area at the bottom of the screen, CSS px; the clone's screen has the rest
 *  (507 px on the Duo, the layout's monitor at 2×). */
const KEYS_HEIGHT = 444;
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
    else if (kind === "up") pad.up(id);
    else pad.cancel(id);
  }, []);
  // A click is a key to the modifiers: Ctrl, then a click, is a Ctrl-click
  const onButton = useCallback((button: number, pressed: boolean) => {
    if (pressed) modifiers.current!.keyDown();
    send({ kind: "button", button, pressed });
    if (!pressed) withMods((m) => m.keyDone());
  }, []);

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

  return (
    <div className="absolute inset-0 bg-black" onPointerDownCapture={claim}>
      <div className="relative overflow-hidden" style={{ height: height - KEYS_HEIGHT }}>
        {/* The session puts the video in here; the cursor and the note come after it, on top */}
        <div ref={box} className="absolute" style={place} />
        <div className="pointer-events-none absolute" style={place}>
          {shape && !hidden && !terminal && connected ? (
            <Cursor shape={shape} x={pos.x} y={pos.y} scale={fit.scale} />
          ) : null}
        </div>
        {note ? (
          <p className="absolute inset-0 flex items-center justify-center text-sm text-neutral-400">
            {note}
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
      />
    </div>
  );
}
