// The /ng page on the iPhone Duo: the selected clone's screen on top, the keyboard below. This
// is the container: it holds the connection (`./session.ts`), the keyboard's state (held
// modifiers, Fn, Caps Lock) and the red dot's cursor motion, and turns them into input.
import { useCallback, useEffect, useRef, useState } from "react";

import { activateLayout } from "~/lib/api";
import type { CursorShape } from "~/lib/wire/CursorShape";
import type { InputMsg } from "~/lib/wire/InputMsg";
import type { ViewSpec } from "~/lib/wire/ViewSpec";

import { Cursor } from "./Cursor";
import { KeyboardView } from "./KeyboardView";
import { CAPS_LOCK } from "./keyboard";
import { clampTo, dotVelocity } from "./pointer";
import { MONITOR, type Session, openSession } from "./session";

/** The built-in layout of the iPhone Duo (`wire::IPHONE_DUO_LAYOUT`). */
const DUO_LAYOUT = "iPhone Duo";
/** Its one monitor, in pixels (`wire::iphone_duo_layout`). */
export const DUO_MONITOR = { width: 1002, height: 810 };
/** Screen (device) pixels per monitor pixel, each way: text is twice its 1:1 size. */
const SCALE = 2;
/** The video's box in device pixels: the monitor at SCALE. */
const BOX = { width: DUO_MONITOR.width * SCALE, height: DUO_MONITOR.height * SCALE };
/** After the red dot moved the cursor, the clone's own cursor reports are this far behind
 *  (ms), so they do not pull the drawn cursor back. */
const OWN_MOVES_MS = 400;

const isDuoSpec = (s: ViewSpec) =>
  s.monitors.length === 1 &&
  s.monitors[0].width === DUO_MONITOR.width &&
  s.monitors[0].height === DUO_MONITOR.height;

type Props = {
  /** The screen area: the top of the screen, under the clock bar too. CSS px. */
  screenHeight: number;
  keysHeight: number;
  /** The home bar's safe-area inset, CSS px. */
  bottom: number;
};

export function Desk({ screenHeight, keysHeight, bottom }: Props) {
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
  /** The cursor where this page last put it, and when. */
  const cursor = useRef({ ...pos, movedAt: -Infinity });
  /** The finger's push on the red dot; null while no finger is on it. */
  const push = useRef<{ dx: number; dy: number } | null>(null);
  const frame = useRef(0);

  const send = (msg: InputMsg) => session.current?.send(msg);

  useEffect(() => {
    if (!box.current) return;
    let asked = false;
    session.current = openSession(box.current, BOX, {
      onSpec(s) {
        setSpec(s);
        // This page needs the iPhone Duo layout; ask once per open, and only when another is
        // active (activating re-applies the layout to the clone).
        if (!asked && !isDuoSpec(s)) {
          asked = true;
          activateLayout(DUO_LAYOUT).catch(() => {});
        }
      },
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
      cancelAnimationFrame(frame.current);
      session.current?.detach();
      session.current = null;
    };
  }, []);

  /** Moves the cursor while a finger pushes the red dot, once per screen frame. */
  const runDot = (last: number) => (t: number) => {
    const p = push.current;
    if (!p) return;
    const dt = Math.min((t - last) / 1000, 0.05);
    // In screen pixels per second, so a push moves the cursor as fast at any SCALE
    const { vx, vy } = dotVelocity(p.dx, p.dy);
    if (vx || vy) {
      const c = cursor.current;
      const x = clampTo(c.x + (vx / SCALE) * dt, DUO_MONITOR.width);
      const y = clampTo(c.y + (vy / SCALE) * dt, DUO_MONITOR.height);
      cursor.current = { x, y, movedAt: performance.now() };
      setPos({ x, y });
      send({ kind: "pointer_move", monitor_id: MONITOR, x, y });
    }
    frame.current = requestAnimationFrame(runDot(t));
  };

  const onDot = useCallback((p: { dx: number; dy: number } | null) => {
    const starting = push.current === null && p !== null;
    push.current = p;
    if (starting) frame.current = requestAnimationFrame(runDot(performance.now()));
  }, []);

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
  const onButton = useCallback(
    (button: number, pressed: boolean) => send({ kind: "button", button, pressed }),
    [],
  );
  const onScroll = useCallback((step: number) => send({ kind: "axis", axis: 0, step }), []);
  const onFn = useCallback(() => setFn((f) => !f), []);
  const terminal = spec?.content.mode === "terminal";
  const note = !connected
    ? "Connecting…"
    : spec && spec.monitors.length === 0
      ? "No clone is selected."
      : terminal
        ? "This clone has no desktop."
        : spec && !isDuoSpec(spec)
          ? "Switching to the iPhone Duo layout…"
          : null;

  return (
    <div className="absolute inset-0 bg-black">
      <div className="relative overflow-hidden" style={{ height: screenHeight }}>
        {/* The session puts the video in here; the cursor and the note come after it, on top */}
        <div ref={box} className="absolute inset-0" />
        {shape && !hidden && !terminal && connected ? <Cursor shape={shape} x={pos.x} y={pos.y} scale={SCALE} /> : null}
        {note ? (
          <p className="absolute inset-0 flex items-center justify-center text-sm text-slate-400">{note}</p>
        ) : null}
      </div>
      <div style={{ height: keysHeight }}>
        <KeyboardView
          fn={fn}
          latched={latched}
          caps={caps}
          bottom={bottom}
          onKey={onKey}
          onModifier={onModifier}
          onButton={onButton}
          onScroll={onScroll}
          onFn={onFn}
          onDot={onDot}
        />
      </div>
    </div>
  );
}
