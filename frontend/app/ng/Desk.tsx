// The /ng page on the iPhone Duo: the selected clone's screen on top, the keyboard below. This
// is the container: it holds the connection (`./session.ts`), the keyboard's state (held
// modifiers, Fn, Caps Lock, keys or trackpad), the trackpad's gestures (`./trackpad.ts`) and
// the split between the two areas, and turns them into input.
import { useCallback, useEffect, useRef, useState } from "react";

import { layoutClaimer } from "~/lib/layoutClaim";
import type { CursorShape } from "~/lib/wire/CursorShape";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { ViewSpec } from "~/lib/wire/ViewSpec";

import { Cursor } from "./Cursor";
import { KeyboardView, type PadMode } from "./KeyboardView";
import { SplitDialog } from "./SplitDialog";
import { naturalKeysHeight, screenFit } from "./geometry";
import { CAPS_LOCK } from "./keyboard";
import { SCROLL_GAIN, clampTo, pointerGain } from "./pointer";
import { MONITOR, type Session, openSession } from "./session";
import { type PadOut, Trackpad } from "./trackpad";

/** The iPhone Duo layout's one monitor, in pixels (`wire::iphone_duo_layout`). */
export const DUO_MONITOR = { width: 1002, height: 810 };
/** After this page moved the cursor, the clone's own cursor reports are this far behind (ms),
 *  so they do not pull the drawn cursor back. */
const OWN_MOVES_MS = 400;
/** A press of the Trackpad button shorter than this switches mode; a longer one is a hold:
 *  trackpad while it is down, keys again when it lifts. */
const HOLD_MS = 300;
/** Where the split was left (the keyboard area's height), kept on this phone only. */
const SPLIT_KEY = "ng-keys-height";
/** The split dialog's range: the least keyboard area, and the least screen. */
const MIN_KEYS = 150;
const MIN_SCREEN = 200;
/** Mutter axis flags: a finger scroll, and its end. */
const AXIS_FINGER = 4;
const AXIS_FINISH = 1;

const isDuoSpec = (s: ViewSpec) =>
  s.monitors.length === 1 &&
  s.monitors[0].width === DUO_MONITOR.width &&
  s.monitors[0].height === DUO_MONITOR.height;

function savedKeysHeight(fallback: number): number {
  try {
    const v = Number(localStorage.getItem(SPLIT_KEY));
    if (v > 0) return v;
  } catch {
    // no storage: the default
  }
  return fallback;
}

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
  const [latched, setLatched] = useState<ReadonlySet<number>>(new Set());
  const [fn, setFn] = useState(false);
  const [caps, setCaps] = useState(false);
  const [mode, setMode] = useState<PadMode>("keys");
  const natural = naturalKeysHeight(width, bottom);
  const [keysHeight, setKeysHeight] = useState(() => savedKeysHeight(natural));
  const [splitOpen, setSplitOpen] = useState(false);
  /** The cursor where this page last put it, and when. */
  const cursor = useRef({ ...pos, movedAt: -Infinity });

  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  const fit = screenFit(width, height - keysHeight, DUO_MONITOR.width, DUO_MONITOR.height, dpr);
  /** Device px per monitor px now, for the trackpad's moves. */
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

  // The video's box in device pixels follows the split
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

  /** The trackpad, made once: what it recognizes goes to the clone. */
  const pad = useRef<Trackpad | null>(null);
  if (!pad.current) {
    const out = (o: PadOut) => {
      switch (o.t) {
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
        case "button":
          send({ kind: "button", button: o.button, pressed: o.pressed });
          break;
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
    pad.current = new Trackpad(out, {
      after(ms, f) {
        const id = setTimeout(f, ms);
        return () => clearTimeout(id);
      },
    });
  }

  // Stable, so the keyboard does not render again each time the cursor moves
  const latchedRef = useRef(latched);
  latchedRef.current = latched;
  const onModifier = useCallback((code: number) => {
    const down = !latchedRef.current.has(code);
    send({ kind: "key_code", keycode: code, pressed: down });
    const next = new Set(latchedRef.current);
    if (down) next.add(code);
    else next.delete(code);
    setLatched(next);
  }, []);
  const onKey = useCallback((code: number, pressed: boolean) => {
    if (code === CAPS_LOCK && pressed) setCaps((c) => !c);
    send({ kind: "key_code", keycode: code, pressed });
  }, []);
  const onFn = useCallback(() => setFn((f) => !f), []);
  const onPad = useCallback((kind: "down" | "move" | "up", id: number, x: number, y: number) => {
    const t = performance.now();
    if (kind === "down") pad.current!.down(id, x, y, t);
    else if (kind === "move") pad.current!.move(id, x, y, t);
    else pad.current!.up(id, t);
  }, []);
  const modeRef = useRef(mode);
  modeRef.current = mode;
  /** When the Trackpad button went down, and the mode before. */
  const press = useRef<{ at: number; before: PadMode }>({ at: 0, before: "keys" });
  const onTrackpadButton = useCallback((down: boolean) => {
    if (down) {
      press.current = { at: performance.now(), before: modeRef.current };
      setMode("trackpad");
      return;
    }
    const { at, before } = press.current;
    const tap = performance.now() - at < HOLD_MS;
    const next: PadMode = tap ? (before === "trackpad" ? "keys" : "trackpad") : before;
    if (next === "keys") pad.current!.cancel();
    setMode(next);
  }, []);
  const onSplit = useCallback(() => setSplitOpen(true), []);
  const onSplitChange = (v: number) => {
    setKeysHeight(v);
    try {
      localStorage.setItem(SPLIT_KEY, String(v));
    } catch {
      // no storage: back to the default next time
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

  return (
    <div className="absolute inset-0 bg-black" onPointerDownCapture={claim}>
      <div className="relative overflow-hidden" style={{ height: height - keysHeight }}>
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
        height={keysHeight}
        bottom={bottom}
        fn={fn}
        latched={latched}
        caps={caps}
        mode={mode}
        onKey={onKey}
        onModifier={onModifier}
        onFn={onFn}
        onTrackpadButton={onTrackpadButton}
        onPad={onPad}
        onSplit={onSplit}
      />
      {splitOpen ? (
        <SplitDialog
          value={keysHeight}
          min={MIN_KEYS}
          max={height - MIN_SCREEN}
          total={height}
          natural={natural}
          onChange={onSplitChange}
          onClose={() => setSplitOpen(false)}
        />
      ) : null}
    </div>
  );
}
