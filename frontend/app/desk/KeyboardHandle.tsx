// The bar along the top of the /ng keyboard (`./FullDesk.tsx`): a long press on it, then a drag
// up or down, makes the keyboard taller or shorter. A drag with no long press first does
// nothing, so a finger that only brushes it changes nothing.
import { type PointerEvent, useRef, useState } from "react";

/** The bar's height, CSS px. */
export const HANDLE_H = 16;
/** A press this long arms the drag. */
const LONG_PRESS_MS = 350;
/** A finger that moves this far (CSS px) before then is no long press. */
const SLOP_PX = 8;

export function KeyboardHandle({
  height,
  onHeight,
  onDone,
}: {
  /** The keyboard's height now, CSS px. */
  height: number;
  /** The drag asks for this height. */
  onHeight: (height: number) => void;
  /** The drag ended. */
  onDone: () => void;
}) {
  const [armed, setArmed] = useState(false);
  const press = useRef<{ id: number; y: number; height: number; timer: ReturnType<typeof setTimeout> } | null>(null);

  const end = () => {
    if (press.current) clearTimeout(press.current.timer);
    press.current = null;
    if (armed) {
      setArmed(false);
      onDone();
    }
  };
  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      aria-label="Keyboard height: long press, then drag"
      className="relative flex items-center justify-center"
      style={{ height: HANDLE_H }}
      onPointerDown={(e: PointerEvent<HTMLDivElement>) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        if (press.current) clearTimeout(press.current.timer);
        press.current = { id: e.pointerId, y: e.clientY, height, timer: setTimeout(() => setArmed(true), LONG_PRESS_MS) };
      }}
      onPointerMove={(e) => {
        const p = press.current;
        if (!p || p.id !== e.pointerId) return;
        const dy = e.clientY - p.y;
        if (armed) onHeight(p.height - dy);
        else if (Math.abs(dy) > SLOP_PX) end();
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onContextMenu={(e) => e.preventDefault()}
    >
      <span
        aria-hidden
        className={`h-1 rounded-full transition-all duration-150 ${armed ? "w-16 bg-black/70 dark:bg-white" : "w-10 bg-black/25 dark:bg-white/30"}`}
      />
    </div>
  );
}
