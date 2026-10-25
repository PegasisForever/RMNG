import { ArrowBigUp, ArrowBigUpDash, ArrowRightToLine, CornerDownLeft } from "lucide-react";
import { type PointerEvent, type ReactNode, memo, useRef, useState } from "react";

import { GAP, keysLayout } from "./geometry";
import { CAPS_LOCK, type Key, SHIFTS, keyLabel, keyRows } from "./keyboard";
import { SLOP_PX } from "./keypad";
import { type NgTab, type ServerUsage, ToolRow } from "./ToolRow";
import type { ModState } from "./modifiers";

/** Evdev mouse buttons. */
export const BTN_LEFT = 0x110;
export const BTN_RIGHT = 0x111;
export const BTN_MIDDLE = 0x112;


export type TouchKind = "down" | "move" | "up" | "cancel";

type Props = {
  /** The keyboard area, CSS px, and the space at its bottom the home bar is over. */
  width: number;
  height: number;
  bottom: number;
  /** Fn: F1–F12 in place of the number row, Home/PgUp/PgDn/End on the arrows. */
  fn: boolean;
  /** The modifiers that are down (evdev code → held, tapped, or locked). */
  mods: ReadonlyMap<number, ModState>;
  /** Caps Lock is on (only the labels follow it). */
  caps: boolean;
  /** A finger on the keys: its id, the key it went down on (null between keys), and where it
   *  is (CSS px, page coordinates). The container tells a tap from a hold or a pointer
   *  gesture (`./keypad.ts`). */
  onKeyTouch: (kind: TouchKind, id: number, key: Key | null, x: number, y: number) => void;
  /** A mouse button goes down or up (evdev code). */
  onButton: (button: number, pressed: boolean) => void;
  /** The server's CPU, memory and disk, for the tool row; null until known. */
  usage: ServerUsage | null;
  /** The open tab, whose page shows in place of the keys and the mouse buttons; null for the
   *  keyboard. */
  tab: NgTab | null;
  onTab: (tab: NgTab) => void;
  /** A count on a tab (clones that stopped working unseen). */
  badges?: Partial<Record<NgTab, number>>;
  /** The keys are in use as a trackpad: they fade to a trackpad's outline, and back. */
  pad: boolean;
};

/** The /ng keyboard area: a tool row under the screen (`./ToolRow.tsx`), a full keyboard of
 *  square keys under it, and the left, middle and right mouse buttons in the space below the
 *  keys; an open tab's page takes the place of the keys and the buttons. The keys are a touchpad too: a finger that moves on them moves the cursor, two
 *  scroll. Each finger works on its own, so a mouse button can stay down while another finger
 *  moves the cursor on the keys (a drag). The mouse buttons show only while the keys are a
 *  trackpad; in keyboard mode their space stays empty, so the keys do not move. */
export const KeyboardView = memo(function KeyboardView(p: Props) {
  const k = keysLayout(p.width, p.height, p.bottom);
  const shift = SHIFTS.some((c) => p.mods.has(c));
  const rows = keyRows(p.fn);
  const touch = useKeysTouch(rows, p.onKeyTouch);
  return (
    <div className="relative touch-none select-none bg-black" style={{ width: p.width, height: p.height }}>
      <ToolRow box={k.tools} usage={p.usage} tab={p.tab} onTab={p.onTab} badges={p.badges} />
      {/* An open tab's page is the container's, over the space the keys leave (`k.page`) */}
      {/* One handler for the whole key area, gaps too: a drag may start between keys */}
      {/* The trackpad the keys fade into: four corners of an outline. Touches go through it
          to the keys, which are the trackpad. */}
      <PadOutline box={k.keys} shown={p.pad && !p.tab} />
      <div
        className={`absolute flex flex-col transition-opacity duration-200 ${p.tab ? "hidden" : ""} ${
          p.pad ? "opacity-0" : "opacity-100"
        }`}
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
                size={Math.min(k.rowHeights[i], k.pitch - GAP)}
                label={keyLabel(key, shift, p.caps)}
                state={keyState(key, p)}
                down={touch.pressed.has(key.id)}
              />
            ))}
          </div>
        ))}
      </div>
      {/* Down to the bottom edge, home bar included: a tap there is a tap. Only in trackpad
          mode; the keypad keeps that mode on while a button is held. */}
      <div
        aria-hidden={!p.pad}
        className={`absolute flex transition-opacity duration-200 ${p.tab ? "hidden" : ""} ${
          p.pad ? "opacity-100" : "pointer-events-none opacity-0"
        }`}
        style={k.buttons}
      >
        <MouseButton label="Left" button={BTN_LEFT} onButton={p.onButton} />
        <MouseButton label="Middle" button={BTN_MIDDLE} onButton={p.onButton} divider />
        <MouseButton label="Right" button={BTN_RIGHT} onButton={p.onButton} divider />
      </div>
    </div>
  );
});

const KEY = "relative flex shrink-0 items-center justify-center rounded-md leading-none";

/** What a key shows it is: a modifier's state, Fn on, or Caps Lock on (lit like a Shift that is
 *  down). Caps Lock's state is the page's count of its presses, so a clone that turns it on or
 *  off by itself goes unseen. */
function keyState(key: Key, p: Props): ModState | undefined {
  if (key.fn) return p.fn ? "latched" : undefined;
  if (key.code === CAPS_LOCK) return p.caps ? "latched" : undefined;
  return p.mods.get(key.code);
}

function tone(down: boolean, active = false) {
  if (active) return "bg-neutral-200 text-black";
  return down ? "bg-neutral-600 text-white" : "bg-neutral-800 text-neutral-100";
}

/** A touch stays with the element it went down on by itself; a mouse is captured so that it
 *  does too. */
function capture(e: PointerEvent<HTMLElement>) {
  if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
}

/** The key area's touches: which key each finger went down on (by `data-key`; none between
 *  keys), and which keys show as pressed. */
function useKeysTouch(rows: Key[][], onKeyTouch: Props["onKeyTouch"]) {
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
      const el = (e.target as HTMLElement).closest<HTMLElement>("[data-key]");
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
  size,
  label,
  state,
  down,
}: {
  k: Key;
  width: number;
  /** The key's height, CSS px; the label scales with it. */
  size: number;
  label: string;
  /** A modifier's (or Fn's) state; undefined when it is up. */
  state: ModState | undefined;
  down: boolean;
}) {
  return (
    <div
      role="button"
      aria-label={k.id}
      aria-pressed={k.mod || k.fn || k.code === CAPS_LOCK ? state !== undefined : undefined}
      data-key={k.id}
      data-state={state}
      className={`${KEY} ${tone(down, state !== undefined)}`}
      style={{ width, height: size, fontSize: size * (label.length > 1 ? 0.25 : 0.38) }}
    >
      {k.icon ? (
        <KeyIcon icon={k.icon} size={size * (k.icon === "tab" ? 0.34 : 0.42)} locked={state === "locked"} />
      ) : (
        label
      )}
    </div>
  );
}

/** The icon a key shows in place of its label. A locked Shift shows the Caps Lock arrow. */
function KeyIcon({ icon, size, locked }: { icon: NonNullable<Key["icon"]>; size: number; locked: boolean }) {
  const p = { size, strokeWidth: 1.75, "aria-hidden": true };
  const icons: Record<NonNullable<Key["icon"]>, ReactNode> = {
    tab: <ArrowRightToLine {...p} />,
    enter: <CornerDownLeft {...p} />,
    shift: locked ? <ArrowBigUpDash {...p} /> : <ArrowBigUp {...p} />,
    win: <WinLogo size={size * 0.8} />,
  };
  return icons[icon];
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
  const corner = "absolute h-6 w-6 border-neutral-500";
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
    </div>
  );
}
