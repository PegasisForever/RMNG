import { type PointerEvent, memo, useState } from "react";

import { Haptic } from "./Haptic";
import { GAP, keysLayout } from "./geometry";
import { type Key, SHIFTS, keyLabel, keyRows } from "./keyboard";

/** Evdev mouse buttons. */
export const BTN_LEFT = 0x110;
export const BTN_RIGHT = 0x111;
export const BTN_MIDDLE = 0x112;

/** Buttons kept free in the tool row for later. */
const SPARE_TOOLS = 5;

export type TouchKind = "down" | "move" | "up" | "cancel";

type Props = {
  /** The keyboard area, CSS px, and the space at its bottom the home bar is over. */
  width: number;
  height: number;
  bottom: number;
  /** Fn: F1–F12 in place of the number row, Home/PgUp/PgDn/End on the arrows. */
  fn: boolean;
  /** The modifiers held down (evdev codes). */
  latched: ReadonlySet<number>;
  /** Caps Lock is on (only the labels follow it). */
  caps: boolean;
  /** A finger on a key: its id, the key it went down on, and where it is (CSS px, page
   *  coordinates). The container tells a tap from a hold or a pointer gesture
   *  (`./keypad.ts`). */
  onKeyTouch: (kind: TouchKind, id: number, key: Key, x: number, y: number) => void;
  /** A mouse button goes down or up (evdev code). */
  onButton: (button: number, pressed: boolean) => void;
};

/** The /ng keyboard area: a tool row (spares for now) under the screen, a full keyboard of
 *  square keys under it, and the left, middle and right mouse buttons in the space below the
 *  keys. The keys are a touchpad too: a finger that moves on them moves the cursor, two
 *  scroll. Each finger works on its own, so a mouse button can stay down while another finger
 *  moves the cursor on the keys (a drag). A tap gives a haptic tick on iOS. */
export const KeyboardView = memo(function KeyboardView(p: Props) {
  const k = keysLayout(p.width, p.height, p.bottom);
  const shift = SHIFTS.some((c) => p.latched.has(c));
  return (
    <div className="relative touch-none select-none bg-black" style={{ width: p.width, height: p.height }}>
      <div className="absolute flex" style={{ ...k.tools, gap: GAP }}>
        {Array.from({ length: SPARE_TOOLS }, (_, i) => (
          <div key={i} className="flex-1 rounded-md bg-neutral-900" />
        ))}
      </div>
      <div
        className="absolute flex flex-col"
        style={{ left: k.keys.left, top: k.keys.top, width: k.keys.width, gap: GAP }}
      >
        {keyRows(p.fn).map((row, i) => (
          <div key={i} className="flex" style={{ gap: GAP, height: k.keyHeight }}>
            {row.map((key) => (
              <KeyButton
                key={key.id}
                k={key}
                width={key.units * k.pitch - GAP}
                size={k.keyHeight}
                label={keyLabel(key, shift, p.caps)}
                latched={key.fn ? p.fn : p.latched.has(key.code)}
                onKeyTouch={p.onKeyTouch}
              />
            ))}
          </div>
        ))}
      </div>
      <div className="absolute flex" style={{ ...k.buttons, paddingBottom: p.bottom }}>
        <MouseButton label="Left" button={BTN_LEFT} onButton={p.onButton} />
        <MouseButton label="Middle" button={BTN_MIDDLE} onButton={p.onButton} divider />
        <MouseButton label="Right" button={BTN_RIGHT} onButton={p.onButton} divider />
      </div>
    </div>
  );
});

const KEY = "relative flex shrink-0 items-center justify-center rounded-md leading-none";

function tone(down: boolean, active = false) {
  if (active) return "bg-neutral-200 text-black";
  return down ? "bg-neutral-600 text-white" : "bg-neutral-800 text-neutral-100";
}

/** Touch keeps each finger on what it touched first (the haptic switch), and the tap that
 *  toggles the switch needs that, so only a mouse is captured. */
function capture(e: PointerEvent<HTMLElement>) {
  if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
}

function KeyButton({
  k,
  width,
  size,
  label,
  latched,
  onKeyTouch,
}: {
  k: Key;
  width: number;
  /** The key's height, CSS px; the label scales with it. */
  size: number;
  label: string;
  latched: boolean;
  onKeyTouch: Props["onKeyTouch"];
}) {
  const [down, setDown] = useState(false);
  const report = (kind: TouchKind) => (e: PointerEvent<HTMLDivElement>) => {
    if (kind === "down") {
      capture(e);
      setDown(true);
    } else if (kind !== "move") setDown(false);
    onKeyTouch(kind, e.pointerId, k, e.clientX, e.clientY);
  };
  return (
    <div
      role="button"
      aria-label={k.id}
      aria-pressed={k.mod || k.fn ? latched : undefined}
      className={`${KEY} ${tone(down, latched)}`}
      style={{ width, height: size, fontSize: size * (label.length > 1 ? 0.25 : 0.38) }}
      onPointerDown={report("down")}
      onPointerMove={report("move")}
      onPointerUp={report("up")}
      onPointerCancel={report("cancel")}
    >
      {label}
      <Haptic />
    </div>
  );
}

/** A mouse button: down while the finger is on it. No background; a thin line divides it from
 *  the one on its left. */
function MouseButton({
  label,
  button,
  onButton,
  divider = false,
}: {
  label: string;
  button: number;
  onButton: Props["onButton"];
  divider?: boolean;
}) {
  const [down, setDown] = useState(false);
  const lift = () => {
    if (!down) return;
    setDown(false);
    onButton(button, false);
  };
  return (
    <div
      role="button"
      aria-label={`${label} button`}
      aria-pressed={down}
      className={`relative flex flex-1 items-center justify-center text-sm ${
        down ? "text-white" : "text-neutral-500"
      }`}
      onPointerDown={(e) => {
        capture(e);
        setDown(true);
        onButton(button, true);
      }}
      onPointerUp={lift}
      onPointerCancel={lift}
    >
      {divider ? <div className="absolute inset-y-2 left-0 w-px bg-neutral-700" /> : null}
      {label}
      <Haptic />
    </div>
  );
}
