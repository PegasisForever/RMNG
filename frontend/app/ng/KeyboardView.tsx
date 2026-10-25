import { type PointerEvent, type ReactNode, memo, useRef, useState } from "react";

import { type Key, ROW_UNITS, SHIFTS, keyLabel, keyRows } from "./keyboard";
import { SCROLL_START_PX, scrollSteps } from "./pointer";

/** Evdev mouse buttons. */
export const BTN_LEFT = 0x110;
export const BTN_RIGHT = 0x111;
export const BTN_MIDDLE = 0x112;

/** Width of the button column, CSS px. */
const COLUMN = 76;
/** Space between keys, CSS px. */
const GAP = 4;
/** Buttons kept free in the column for later. */
const SPARE_BUTTONS = 3;
/** The red dot sits between P (second row) and L (third row): halfway between their middles,
 *  in key units from the left, and on the line between the two rows. */
const DOT_UNITS = (11 + 10.25) / 2;

type Props = {
  /** Fn: F1–F12 in place of the number row, Home/PgUp/PgDn/End on the arrows. */
  fn: boolean;
  /** The modifiers held down (evdev codes). */
  latched: ReadonlySet<number>;
  /** Caps Lock is on (only the labels follow it). */
  caps: boolean;
  /** Space kept clear at the bottom for the home bar, CSS px. */
  bottom: number;
  /** A key that is not a modifier goes down or up. */
  onKey: (code: number, pressed: boolean) => void;
  /** A modifier was tapped: it goes down, or up when it was down. */
  onModifier: (code: number) => void;
  /** A mouse button goes down or up (evdev code). */
  onButton: (button: number, pressed: boolean) => void;
  /** Wheel notches from a middle-button drag: positive is down. */
  onScroll: (steps: number) => void;
  onFn: () => void;
  /** The finger on the red dot, as its push from where it first touched (CSS px), or null
   *  when it lifts. */
  onDot: (push: { dx: number; dy: number } | null) => void;
};

/** The /ng keyboard area: a full keyboard, a column of mouse buttons on the right, and a
 *  red dot that moves the cursor (like a ThinkPad TrackPoint). Each finger works on its own,
 *  so a mouse button can stay down while another finger moves the dot. */
export const KeyboardView = memo(function KeyboardView(p: Props) {
  const shift = SHIFTS.some((c) => p.latched.has(c));
  return (
    <div
      className="flex h-full touch-none select-none bg-black px-1 pt-1.5"
      style={{ gap: GAP, paddingBottom: p.bottom }}
    >
      <div className="relative flex min-w-0 flex-1 flex-col" style={{ gap: GAP }}>
        {keyRows(p.fn).map((row, i) => (
          <div key={i} className="flex min-h-0 flex-1" style={{ gap: GAP }}>
            {row.map((k) => (
              <KeyButton
                key={k.id}
                k={k}
                label={keyLabel(k, shift, p.caps)}
                latched={p.latched.has(k.code)}
                onKey={p.onKey}
                onModifier={p.onModifier}
              />
            ))}
          </div>
        ))}
        <RedDot onDot={p.onDot} />
      </div>
      <div className="flex flex-col" style={{ gap: GAP, width: COLUMN }}>
        <Hold label="Left" onPress={(down) => p.onButton(BTN_LEFT, down)} />
        <MiddleButton onButton={p.onButton} onScroll={p.onScroll} />
        <Hold label="Right" onPress={(down) => p.onButton(BTN_RIGHT, down)} />
        <Pad active={p.fn} onDown={p.onFn}>
          Fn
        </Pad>
        {Array.from({ length: SPARE_BUTTONS }, (_, i) => (
          <Pad key={i} />
        ))}
      </div>
    </div>
  );
});

const KEY = "flex min-w-0 items-center justify-center rounded-md font-medium leading-none";

function tone(down: boolean, active = false) {
  if (active) return "bg-blue-600 text-white";
  return down ? "bg-slate-500 text-white" : "bg-slate-800 text-slate-100";
}

function KeyButton({
  k,
  label,
  latched,
  onKey,
  onModifier,
}: {
  k: Key;
  label: string;
  latched: boolean;
  onKey: Props["onKey"];
  onModifier: Props["onModifier"];
}) {
  const [down, setDown] = useState(false);
  const press = (e: PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    setDown(true);
    if (k.mod) onModifier(k.code);
    else onKey(k.code, true);
  };
  const lift = () => {
    if (!down) return;
    setDown(false);
    if (!k.mod) onKey(k.code, false);
  };
  return (
    <div
      role="button"
      aria-label={k.id}
      aria-pressed={k.mod ? latched : undefined}
      className={`${KEY} ${tone(down, latched)} ${label.length > 2 ? "text-sm" : "text-xl"}`}
      style={{ flex: `${k.units} ${k.units} 0` }}
      onPointerDown={press}
      onPointerUp={lift}
      onPointerCancel={lift}
    >
      {label}
    </div>
  );
}

/** A column button that is down while the finger is on it. */
function Hold({ label, onPress }: { label: string; onPress: (down: boolean) => void }) {
  const [down, setDown] = useState(false);
  const lift = () => {
    if (!down) return;
    setDown(false);
    onPress(false);
  };
  return (
    <div
      role="button"
      className={`${KEY} flex-1 text-sm ${tone(down)}`}
      onPointerDown={(e) => {
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        setDown(true);
        onPress(true);
      }}
      onPointerUp={lift}
      onPointerCancel={lift}
    >
      {label}
    </div>
  );
}

/** The middle button: a tap clicks it, a drag up or down scrolls up or down. */
function MiddleButton({ onButton, onScroll }: Pick<Props, "onButton" | "onScroll">) {
  const touch = useRef<{ startY: number; lastY: number; scrolling: boolean } | null>(null);
  const rem = useRef({ y: 0 });
  const [down, setDown] = useState(false);
  const lift = (click: boolean) => {
    const t = touch.current;
    touch.current = null;
    setDown(false);
    if (click && t && !t.scrolling) {
      onButton(BTN_MIDDLE, true);
      onButton(BTN_MIDDLE, false);
    }
  };
  return (
    <div
      role="button"
      className={`${KEY} flex-1 flex-col gap-1 text-sm ${tone(down)}`}
      onPointerDown={(e) => {
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        touch.current = { startY: e.clientY, lastY: e.clientY, scrolling: false };
        rem.current = { y: 0 };
        setDown(true);
      }}
      onPointerMove={(e) => {
        const t = touch.current;
        if (!t) return;
        if (!t.scrolling && Math.abs(e.clientY - t.startY) < SCROLL_START_PX) return;
        t.scrolling = true;
        const steps = scrollSteps(e.clientY - t.lastY, rem.current);
        t.lastY = e.clientY;
        if (steps) onScroll(steps);
      }}
      onPointerUp={() => lift(true)}
      onPointerCancel={() => lift(false)}
    >
      <span>Middle</span>
      <span className="text-xs text-slate-400">↕ scroll</span>
    </div>
  );
}

/** A column button that does something when touched, or nothing yet (a spare). */
function Pad({ active = false, onDown, children }: { active?: boolean; onDown?: () => void; children?: ReactNode }) {
  return (
    <div
      role={onDown ? "button" : undefined}
      aria-pressed={onDown ? active : undefined}
      className={`${KEY} flex-1 text-sm ${onDown ? tone(false, active) : "bg-slate-900"}`}
      onPointerDown={(e) => {
        e.preventDefault();
        onDown?.();
      }}
    >
      {children}
    </div>
  );
}

/** The red dot. It reports how far the finger pushed from where it touched. */
function RedDot({ onDot }: Pick<Props, "onDot">) {
  const origin = useRef<{ x: number; y: number } | null>(null);
  const lift = () => {
    if (!origin.current) return;
    origin.current = null;
    onDot(null);
  };
  return (
    <div
      aria-label="Red dot"
      className="absolute flex h-11 w-11 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full"
      style={{ left: `${(DOT_UNITS / ROW_UNITS) * 100}%`, top: `calc(40% + ${GAP * 0.7}px)` }}
      onPointerDown={(e) => {
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        origin.current = { x: e.clientX, y: e.clientY };
        onDot({ dx: 0, dy: 0 });
      }}
      onPointerMove={(e) => {
        const o = origin.current;
        if (o) onDot({ dx: e.clientX - o.x, dy: e.clientY - o.y });
      }}
      onPointerUp={lift}
      onPointerCancel={lift}
    >
      <div className="h-4 w-4 rounded-full bg-red-600 shadow-[0_0_0_2px_rgba(0,0,0,0.6)]" />
    </div>
  );
}
