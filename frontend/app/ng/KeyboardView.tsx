import { type PointerEvent, memo, useRef, useState } from "react";

import { Haptic } from "./Haptic";
import { GAP, keysLayout } from "./geometry";
import { type Key, SHIFTS, keyLabel, keyRows } from "./keyboard";
import { SLOP_PX } from "./keypad";

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
  /** A finger on the keys: its id, the key it went down on (null between keys), and where it
   *  is (CSS px, page coordinates). The container tells a tap from a hold or a pointer
   *  gesture (`./keypad.ts`). */
  onKeyTouch: (kind: TouchKind, id: number, key: Key | null, x: number, y: number) => void;
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
  const rows = keyRows(p.fn);
  const touch = useKeysTouch(rows, p.onKeyTouch);
  return (
    <div className="relative touch-none select-none bg-black" style={{ width: p.width, height: p.height }}>
      <div className="absolute flex" style={{ ...k.tools, gap: GAP }}>
        {Array.from({ length: SPARE_TOOLS }, (_, i) => (
          <div key={i} className="flex-1 rounded-md bg-neutral-900" />
        ))}
      </div>
      {/* One handler for the whole key area, gaps too: a drag may start between keys */}
      <div
        className="absolute flex flex-col"
        style={{ ...k.keys, gap: GAP }}
        onPointerDown={touch.down}
        onPointerMove={touch.move}
        onPointerUp={touch.up("up")}
        onPointerCancel={touch.up("cancel")}
      >
        {rows.map((row, i) => (
          <div key={i} className="flex" style={{ gap: GAP, height: k.keyHeight }}>
            {row.map((key) => (
              <KeyButton
                key={key.id}
                k={key}
                width={key.units * k.pitch - GAP}
                size={k.keyHeight}
                label={keyLabel(key, shift, p.caps)}
                latched={key.fn ? p.fn : p.latched.has(key.code)}
                down={touch.pressed.has(key.id)}
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

/** The key area's touches: which key each finger went down on (by `data-key`; none between
 *  keys), which keys show as pressed, and the haptic switch of a finger that drags. */
function useKeysTouch(rows: Key[][], onKeyTouch: Props["onKeyTouch"]) {
  const [pressed, setPressed] = useState<ReadonlySet<string>>(new Set());
  /** Each finger: its key, where it went down, and its key's switch. */
  const fingers = useRef(new Map<number, { key: Key | null; x: number; y: number; sw: HTMLInputElement | null }>());
  const byId = new Map(rows.flat().map((k) => [k.id, k]));
  const show = (id: string, on: boolean) =>
    setPressed((s) => {
      const next = new Set(s);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  return {
    pressed,
    down(e: PointerEvent<HTMLDivElement>) {
      capture(e);
      const el = (e.target as HTMLElement).closest<HTMLElement>("[data-key]");
      const key = (el && byId.get(el.dataset.key!)) ?? null;
      const sw = el?.querySelector<HTMLInputElement>("input[switch]") ?? null;
      fingers.current.set(e.pointerId, { key, x: e.clientX, y: e.clientY, sw });
      if (key) show(key.id, true);
      onKeyTouch("down", e.pointerId, key, e.clientX, e.clientY);
    },
    move(e: PointerEvent<HTMLDivElement>) {
      const f = fingers.current.get(e.pointerId);
      if (!f) return;
      // A finger that drags is no tap: its key stops looking pressed, and its switch stops
      // working, as a switch toggles (and ticks) when a finger slides across it
      if (f.sw && !f.sw.disabled && Math.hypot(e.clientX - f.x, e.clientY - f.y) >= SLOP_PX) {
        f.sw.disabled = true;
        if (f.key) show(f.key.id, false);
      }
      onKeyTouch("move", e.pointerId, f.key, e.clientX, e.clientY);
    },
    up: (kind: "up" | "cancel") => (e: PointerEvent<HTMLDivElement>) => {
      const f = fingers.current.get(e.pointerId);
      if (!f) return;
      fingers.current.delete(e.pointerId);
      if (f.key) show(f.key.id, false);
      // On again once the click that ends this touch has gone by
      const sw = f.sw;
      if (sw?.disabled) setTimeout(() => (sw.disabled = false), 100);
      onKeyTouch(kind, e.pointerId, f.key, e.clientX, e.clientY);
    },
  };
}

function KeyButton({
  k,
  width,
  size,
  label,
  latched,
  down,
}: {
  k: Key;
  width: number;
  /** The key's height, CSS px; the label scales with it. */
  size: number;
  label: string;
  latched: boolean;
  down: boolean;
}) {
  return (
    <div
      role="button"
      aria-label={k.id}
      aria-pressed={k.mod || k.fn ? latched : undefined}
      data-key={k.id}
      className={`${KEY} ${tone(down, latched)}`}
      style={{ width, height: size, fontSize: size * (label.length > 1 ? 0.25 : 0.38) }}
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
