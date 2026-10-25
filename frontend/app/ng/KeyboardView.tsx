import { type PointerEvent, type ReactNode, memo, useState } from "react";

import { GAP, TOOL_H, keysLayout } from "./geometry";
import { type Key, SHIFTS, keyLabel, keyRows } from "./keyboard";

/** What the key area is: the keys, or a trackpad. */
export type PadMode = "keys" | "trackpad";

/** Spare buttons kept free in the tool row for later. */
const SPARE_BUTTONS = 3;

type Props = {
  /** The keyboard area, CSS px, and the space kept clear at its bottom for the home bar. */
  width: number;
  height: number;
  bottom: number;
  /** Fn: F1–F12 in place of the number row, Home/PgUp/PgDn/End on the arrows. */
  fn: boolean;
  /** The modifiers held down (evdev codes). */
  latched: ReadonlySet<number>;
  /** Caps Lock is on (only the labels follow it). */
  caps: boolean;
  mode: PadMode;
  /** A key that is not a modifier goes down or up. */
  onKey: (code: number, pressed: boolean) => void;
  /** A modifier was tapped: it goes down, or up when it was down. */
  onModifier: (code: number) => void;
  onFn: () => void;
  /** The Trackpad button goes down or up (the container tells a tap from a hold). */
  onTrackpadButton: (down: boolean) => void;
  /** A finger on the trackpad: its id and where it is (CSS px, page coordinates). */
  onPad: (kind: "down" | "move" | "up", id: number, x: number, y: number) => void;
  /** Open the keyboard / screen split dialog. */
  onSplit: () => void;
};

/** The /ng keyboard area: a tool row over a full keyboard with square keys, at the bottom of
 *  the area. In trackpad mode the keys' place is a trackpad; the tool row stays. Each finger
 *  works on its own, so the Trackpad button can be held while other fingers use the pad. */
export const KeyboardView = memo(function KeyboardView(p: Props) {
  const k = keysLayout(p.width, p.height, p.bottom);
  const shift = SHIFTS.some((c) => p.latched.has(c));
  const keyH = k.pitch - GAP;
  return (
    <div className="relative touch-none select-none bg-black" style={{ width: p.width, height: p.height }}>
      <div
        className="absolute flex"
        style={{ left: k.left, top: k.top - GAP - TOOL_H, width: k.width, height: TOOL_H, gap: GAP }}
      >
        <Tool
          grow={2}
          active={p.mode === "trackpad"}
          onDown={() => p.onTrackpadButton(true)}
          onUp={() => p.onTrackpadButton(false)}
        >
          Trackpad
        </Tool>
        <Tool onDown={p.onSplit}>Split</Tool>
        {Array.from({ length: SPARE_BUTTONS }, (_, i) => (
          <Tool key={i} />
        ))}
      </div>
      {p.mode === "trackpad" ? (
        <TrackpadSurface box={k} onPad={p.onPad} />
      ) : (
        <div className="absolute flex flex-col" style={{ left: k.left, top: k.top, width: k.width, gap: GAP }}>
          {keyRows(p.fn).map((row, i) => (
            <div key={i} className="flex" style={{ gap: GAP, height: keyH }}>
              {row.map((key) => (
                <KeyButton
                  key={key.id}
                  k={key}
                  width={key.units * k.pitch - GAP}
                  size={keyH}
                  label={keyLabel(key, shift, p.caps)}
                  latched={key.fn ? p.fn : p.latched.has(key.code)}
                  onKey={p.onKey}
                  onModifier={p.onModifier}
                  onFn={p.onFn}
                />
              ))}
            </div>
          ))}
        </div>
      )}
    </div>
  );
});

const KEY = "flex shrink-0 items-center justify-center rounded-md leading-none";

function tone(down: boolean, active = false) {
  if (active) return "bg-neutral-200 text-black";
  return down ? "bg-neutral-600 text-white" : "bg-neutral-800 text-neutral-100";
}

function KeyButton({
  k,
  width,
  size,
  label,
  latched,
  onKey,
  onModifier,
  onFn,
}: {
  k: Key;
  width: number;
  /** The key's height, CSS px; the label scales with it. */
  size: number;
  label: string;
  latched: boolean;
  onKey: Props["onKey"];
  onModifier: Props["onModifier"];
  onFn: Props["onFn"];
}) {
  const [down, setDown] = useState(false);
  const press = (e: PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    setDown(true);
    if (k.fn) onFn();
    else if (k.mod) onModifier(k.code);
    else onKey(k.code, true);
  };
  const lift = () => {
    if (!down) return;
    setDown(false);
    if (!k.mod && !k.fn) onKey(k.code, false);
  };
  return (
    <div
      role="button"
      aria-label={k.id}
      aria-pressed={k.mod || k.fn ? latched : undefined}
      className={`${KEY} ${tone(down, latched)}`}
      style={{ width, height: size, fontSize: size * (label.length > 1 ? 0.3 : 0.45) }}
      onPointerDown={press}
      onPointerUp={lift}
      onPointerCancel={lift}
    >
      {label}
    </div>
  );
}

/** A tool-row button: `onDown` when touched, `onUp` when the finger lifts. Without either it is
 *  a spare. */
function Tool({
  grow = 1,
  active = false,
  onDown,
  onUp,
  children,
}: {
  grow?: number;
  active?: boolean;
  onDown?: () => void;
  onUp?: () => void;
  children?: ReactNode;
}) {
  const [down, setDown] = useState(false);
  const lift = () => {
    if (!down) return;
    setDown(false);
    onUp?.();
  };
  return (
    <div
      role={onDown ? "button" : undefined}
      aria-pressed={onUp ? active : undefined}
      className={`${KEY} text-sm ${onDown ? tone(down, active) : "bg-neutral-900"}`}
      style={{ flex: `${grow} ${grow} 0` }}
      onPointerDown={(e) => {
        e.preventDefault();
        if (!onDown) return;
        e.currentTarget.setPointerCapture(e.pointerId);
        setDown(true);
        onDown();
      }}
      onPointerUp={lift}
      onPointerCancel={lift}
    >
      {children}
    </div>
  );
}

/** The trackpad, where the keys are. It reports each finger. */
function TrackpadSurface({
  box,
  onPad,
}: {
  box: { left: number; top: number; width: number; height: number };
  onPad: Props["onPad"];
}) {
  const report = (kind: "down" | "move" | "up") => (e: PointerEvent<HTMLDivElement>) => {
    if (kind === "down") {
      e.preventDefault();
      e.currentTarget.setPointerCapture(e.pointerId);
    }
    onPad(kind, e.pointerId, e.clientX, e.clientY);
  };
  return (
    <div
      aria-label="Trackpad"
      className="absolute flex items-center justify-center rounded-xl border border-neutral-800 bg-neutral-950 text-sm text-neutral-600"
      style={{ left: box.left, top: box.top, width: box.width, height: box.height }}
      onPointerDown={report("down")}
      onPointerMove={report("move")}
      onPointerUp={report("up")}
      onPointerCancel={report("up")}
    >
      Trackpad
    </div>
  );
}
