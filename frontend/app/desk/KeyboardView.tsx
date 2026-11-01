import {
  ArrowBigUp,
  ArrowBigUpDash,
  ArrowDown,
  ArrowRightToLine,
  ArrowUp,
  Clipboard,
  CornerDownLeft,
  Mic,
  MicOff,
} from "lucide-react";
import { type PointerEvent, type ReactNode, memo, useRef, useState } from "react";

import type { DictationState } from "./dictation";
import { GAP, keysLayout } from "./geometry";
import { CAPS_LOCK, type Key, SHIFTS, keyLabel, keyRows } from "./keyboard";
import { SLOP_PX } from "./keypad";
import type { ModState } from "./modifiers";

/** Evdev mouse buttons. */
export const BTN_LEFT = 0x110;
export const BTN_RIGHT = 0x111;
export const BTN_MIDDLE = 0x112;


export type TouchKind = "down" | "move" | "up" | "cancel";

export type KeyboardViewProps = {
  /** The keyboard area, CSS px, and the space at its bottom the home bar is over. */
  width: number;
  height: number;
  bottom: number;
  /** Fn: F1–F12 in place of the symbol row; Home, PgUp, PgDn, End, the clipboard keys and the
   *  arrows on the number row (`keyRows`). */
  fn: boolean;
  /** The modifiers that are down (evdev code → held, tapped, or locked). */
  mods: ReadonlyMap<number, ModState>;
  /** Caps Lock is on (only the labels follow it). */
  caps: boolean;
  /** A finger on the keys: its id, the key it went down on (the nearest one for a touch between
   *  keys; null only for an area with no keys), and where it
   *  is (CSS px, page coordinates). The container tells a tap from a hold or a pointer
   *  gesture (`./keypad.ts`). */
  onKeyTouch: (kind: TouchKind, id: number, key: Key | null, x: number, y: number) => void;
  /** The keys are in use as a trackpad: they fade to a trackpad's outline, and back. */
  pad: boolean;
  /** What the dictation key shows: off, starting, listening (red), a fault, or no dictation in
   *  this browser (dim). */
  dictation: DictationState;
};

/** The on-screen keyboard: a full keyboard of square keys (`./FullDesk.tsx` puts it over the clone's
 *  screen or under it). The keys are a touchpad too: a finger that moves on them moves the
 *  cursor, two scroll. */
export const KeyboardView = memo(function KeyboardView(p: KeyboardViewProps) {
  const k = keysLayout(p.width, p.height, p.bottom);
  const shift = SHIFTS.some((c) => p.mods.has(c));
  const rows = keyRows(p.fn);
  const touch = useKeysTouch(rows, p.onKeyTouch);
  return (
    <div className="relative touch-none select-none" style={{ width: p.width, height: p.height }}>
      {/* One handler for the whole key area, gaps too: a drag may start between keys */}
      {/* The trackpad the keys fade into: four corners of an outline. Touches go through it
          to the keys, which are the trackpad. */}
      <PadOutline box={k.keys} shown={p.pad} />
      <div
        data-keyboard
        className={`absolute flex flex-col transition-opacity duration-200 ${p.pad ? "opacity-0" : "opacity-100"}`}
        style={{ ...k.keys, gap: GAP }}
        onPointerDown={touch.down}
        onPointerMove={touch.move}
        onPointerUp={touch.up("up")}
        onPointerCancel={touch.up("cancel")}
      >
        {rows.map((row, i) => (
          <div key={i} className="flex" style={{ gap: GAP, height: k.rowHeights[i] }}>
            {row.map((key) => (
              <KeyButton
                key={key.id}
                k={key}
                width={key.units * k.pitch - GAP}
                height={k.rowHeights[i]}
                size={Math.min(k.rowHeights[i], k.pitch - GAP)}
                label={keyLabel(key, shift, p.caps)}
                state={keyState(key, p)}
                down={touch.pressed.has(key.id)}
                dictation={key.dictate ? p.dictation : undefined}
              />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
});

/** The panel the keys are on: glass while it floats over the clone's screen, with a denser tint
 *  (grey in the light look) so that each key shows over a white window or a black one; a solid
 *  panel in the same colors while it is docked under the screen, where there is nothing to see
 *  through it (`./GLASS.md`). */
export function keyboardPanel(docked: boolean) {
  return docked
    ? "bg-neutral-300 text-neutral-900 dark:bg-neutral-950 dark:text-white"
    : "glass glass-thick rounded-t-2xl [--glass-tint:rgb(212_212_212/0.7)] dark:[--glass-tint:rgb(23_23_23/0.82)]";
}

const KEY = "relative flex shrink-0 items-center justify-center rounded-md border border-black/10 leading-none dark:border-transparent";

/** What a key shows it is: a modifier's state, Fn on, or Caps Lock on (lit like a Shift that is
 *  down). Caps Lock's state is the page's count of its presses, so a clone that turns it on or
 *  off by itself goes unseen. */
function keyState(key: Key, p: KeyboardViewProps): ModState | undefined {
  if (key.fn) return p.fn ? "latched" : undefined;
  if (key.code === CAPS_LOCK) return p.caps ? "latched" : undefined;
  return p.mods.get(key.code);
}

/** A key is flat content on the keyboard's panel (`./GLASS.md`), in the light or the dark look.
 *  A key that is on (a modifier, Fn, Caps Lock) takes the other look, as lit glass does. */
const KEY_UP = "bg-white/85 text-neutral-900 dark:bg-white/15 dark:text-white";

function tone(down: boolean, active = false) {
  if (active) return "bg-neutral-800 text-white dark:bg-neutral-100 dark:text-neutral-900";
  return down ? "bg-black/15 text-neutral-900 dark:bg-white/35 dark:text-white" : KEY_UP;
}

/** A touch stays with the element it went down on by itself; a mouse is captured so that it
 *  does too. */
function capture(e: PointerEvent<HTMLElement>) {
  if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
}

/** The key in `area` nearest to the point (page coordinates), or null when it has none. */
function nearestKey(area: HTMLElement, x: number, y: number): HTMLElement | null {
  let best: HTMLElement | null = null;
  let bestD = Infinity;
  for (const el of area.querySelectorAll<HTMLElement>("[data-key]")) {
    const r = el.getBoundingClientRect();
    const d = Math.hypot(Math.max(r.left - x, 0, x - r.right), Math.max(r.top - y, 0, y - r.bottom));
    if (d < bestD) [best, bestD] = [el, d];
  }
  return best;
}

/** The key area's touches: which key each finger went down on (by `data-key`, or the nearest
 *  key for a touch between keys), and which keys show as pressed. */
function useKeysTouch(rows: Key[][], onKeyTouch: KeyboardViewProps["onKeyTouch"]) {
  const [pressed, setPressed] = useState<ReadonlySet<string>>(new Set());
  /** Each finger: its key, where it went down, and whether it moved off as a drag. */
  const fingers = useRef(new Map<number, { key: Key | null; x: number; y: number; dragged: boolean }>());
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
      // A finger between two keys (or in the free half unit after "p") types the nearest one
      const el =
        (e.target as HTMLElement).closest<HTMLElement>("[data-key]") ?? nearestKey(e.currentTarget, e.clientX, e.clientY);
      const key = (el && byId.get(el.dataset.key!)) ?? null;
      fingers.current.set(e.pointerId, { key, x: e.clientX, y: e.clientY, dragged: false });
      if (key) show(key.id, true);
      onKeyTouch("down", e.pointerId, key, e.clientX, e.clientY);
    },
    move(e: PointerEvent<HTMLDivElement>) {
      const f = fingers.current.get(e.pointerId);
      if (!f) return;
      // A finger that drags is no tap: its key stops looking pressed
      if (!f.dragged && Math.hypot(e.clientX - f.x, e.clientY - f.y) >= SLOP_PX) {
        f.dragged = true;
        if (f.key) show(f.key.id, false);
      }
      onKeyTouch("move", e.pointerId, f.key, e.clientX, e.clientY);
    },
    up: (kind: "up" | "cancel") => (e: PointerEvent<HTMLDivElement>) => {
      const f = fingers.current.get(e.pointerId);
      if (!f) return;
      fingers.current.delete(e.pointerId);
      if (f.key) show(f.key.id, false);
      onKeyTouch(kind, e.pointerId, f.key, e.clientX, e.clientY);
    },
  };
}

function KeyButton({
  k,
  width,
  height,
  size,
  label,
  state,
  down,
  dictation,
}: {
  k: Key;
  width: number;
  height: number;
  /** The key's height, or its width where that is less, CSS px; the label scales with it. */
  size: number;
  label: string;
  /** A modifier's (or Fn's) state; undefined when it is up. */
  state: ModState | undefined;
  down: boolean;
  /** The dictation key's state; undefined for every other key. */
  dictation?: DictationState;
}) {
  const iconSize = size * (k.icon === "tab" ? 0.34 : 0.42);
  return (
    <div
      role="button"
      aria-label={k.clip ? k.label : k.id}
      title={k.clip ? k.label : undefined}
      aria-pressed={
        k.mod || k.fn || k.code === CAPS_LOCK ? state !== undefined : dictation ? dictation === "listening" : undefined
      }
      data-key={k.id}
      data-state={state ?? dictation}
      className={`${KEY} ${dictation ? DICTATION_TONE[dictation] : tone(down, state !== undefined)}`}
      style={{ width, height, fontSize: size * (label.length > 1 ? 0.25 : 0.38) }}
    >
      {dictation ? (
        <DictationIcon state={dictation} size={iconSize} />
      ) : k.icon ? (
        <KeyIcon icon={k.icon} size={iconSize} locked={state === "locked"} />
      ) : (
        label
      )}
    </div>
  );
}

/** The dictation key in each state: red while it listens, darker red while it starts, a red
 *  struck-out mic for a fault, dim where the browser has no dictation. */
const DICTATION_TONE: Record<DictationState, string> = {
  off: KEY_UP,
  starting: "bg-rose-200 text-rose-900 dark:bg-rose-900 dark:text-rose-100",
  listening: "bg-rose-600 text-white",
  error: "bg-white/85 text-rose-600 dark:bg-white/15 dark:text-rose-400",
  unsupported: "bg-white/30 text-black/30 dark:bg-white/5 dark:text-white/30",
};

function DictationIcon({ state, size }: { state: DictationState; size: number }) {
  const p = { size, strokeWidth: 1.75, "aria-hidden": true };
  if (state === "error" || state === "unsupported") return <MicOff {...p} />;
  return <Mic {...p} className={state === "listening" ? "animate-pulse" : undefined} />;
}

/** The icon a key shows in place of its label. A locked Shift shows the Caps Lock arrow. */
function KeyIcon({ icon, size, locked }: { icon: NonNullable<Key["icon"]>; size: number; locked: boolean }) {
  const p = { size, strokeWidth: 1.75, "aria-hidden": true };
  const icons: Record<NonNullable<Key["icon"]>, ReactNode> = {
    tab: <ArrowRightToLine {...p} />,
    enter: <CornerDownLeft {...p} />,
    shift: locked ? <ArrowBigUpDash {...p} /> : <ArrowBigUp {...p} />,
    win: <WinLogo size={size * 0.8} />,
    mic: <Mic {...p} />,
    "clip-down": <ClipIcon size={size} arrow={<ArrowDown {...p} size={size * 0.7} />} />,
    "clip-up": <ClipIcon size={size} arrow={<ArrowUp {...p} size={size * 0.7} />} />,
  };
  return icons[icon];
}

/** A clipboard with an arrow beside it: down, the clone's clipboard to this device; up, this
 *  device's to the clone. */
function ClipIcon({ size, arrow }: { size: number; arrow: ReactNode }) {
  return (
    <span className="flex items-center">
      <Clipboard size={size * 0.85} strokeWidth={1.75} aria-hidden />
      {arrow}
    </span>
  );
}

/** The four panes of the Win key. */
function WinLogo({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <rect x="0" y="0" width="7.4" height="7.4" />
      <rect x="8.6" y="0" width="7.4" height="7.4" />
      <rect x="0" y="8.6" width="7.4" height="7.4" />
      <rect x="8.6" y="8.6" width="7.4" height="7.4" />
    </svg>
  );
}

/** The trackpad's outline: an L at each corner of the key area. It fades in and out. */
function PadOutline({ box, shown }: { box: KeysLayoutBox; shown: boolean }) {
  const corner = "absolute h-6 w-6 border-black/35 dark:border-white/40";
  return (
    <div
      aria-hidden
      data-pad={shown}
      className={`pointer-events-none absolute transition-opacity duration-200 ${shown ? "opacity-100" : "opacity-0"}`}
      style={box}
    >
      <div className={`${corner} left-0 top-0 rounded-tl-xl border-l-2 border-t-2`} />
      <div className={`${corner} right-0 top-0 rounded-tr-xl border-r-2 border-t-2`} />
      <div className={`${corner} bottom-0 left-0 rounded-bl-xl border-b-2 border-l-2`} />
      <div className={`${corner} bottom-0 right-0 rounded-br-xl border-b-2 border-r-2`} />
    </div>
  );
}

type KeysLayoutBox = ReturnType<typeof keysLayout>["keys"];
