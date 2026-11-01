// A headless clone's terminal in /ng, as it is drawn (`./NgTerminal.tsx` is its container): the
// tab bar (`./TermBar.tsx`) and, under it, the terminal (`children`: the box xterm.js draws in,
// or a story's picture). In touch mode its area is a trackpad, with the pointer drawn on it.
import type { PointerEvent, ReactNode } from "react";

import type { TouchKind } from "./KeyboardView";
import { TermBar } from "./TermBar";

/** The terminal's background (the xterm.js theme's, `~/viewer/terminal.tsx`). */
export const TERM_BG = "#1c1c1f";

export function NgTerminalView({
  sessions,
  active,
  onPick,
  onNew,
  top,
  touch,
  cursor,
  onPad,
  children,
}: {
  sessions: string[];
  active: string | null;
  onPick: (session: string) => void;
  onNew: () => void;
  /** The clock bar's safe-area inset, CSS px. */
  top: number;
  /** Touch mode: the trackpad and the drawn pointer. */
  touch: boolean;
  /** The drawn pointer, CSS px in the terminal; null: none yet. */
  cursor: { x: number; y: number } | null;
  /** A finger on the terminal's area (touch mode). */
  onPad: (kind: TouchKind, e: PointerEvent<HTMLDivElement>) => void;
  children: ReactNode;
}) {
  const on = (kind: TouchKind) => (e: PointerEvent<HTMLDivElement>) => onPad(kind, e);
  return (
    <div className="absolute inset-0 flex flex-col" style={{ background: TERM_BG }}>
      <TermBar sessions={sessions} active={active} onPick={onPick} onNew={onNew} top={top} />
      {/* A half-cell inset */}
      <div className="relative min-h-0 flex-1 pl-1.5 pt-1">
        <div className="relative h-full w-full">
          {children}
          {touch ? (
            <>
              <div
                aria-label="Trackpad"
                className="absolute inset-0 touch-none"
                onPointerDown={on("down")}
                onPointerMove={on("move")}
                onPointerUp={on("up")}
                onPointerCancel={on("cancel")}
              />
              {cursor ? <Pointer x={cursor.x} y={cursor.y} /> : null}
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}

/** The drawn pointer: an arrow with its tip at `x`, `y`. */
function Pointer({ x, y }: { x: number; y: number }) {
  return (
    <svg
      aria-hidden
      width="14"
      height="21"
      viewBox="0 0 14 21"
      className="pointer-events-none absolute left-0 top-0"
      style={{ transform: `translate(${x}px, ${y}px)` }}
    >
      <path d="M1 1 L1 17 L5 13 L8 20 L11 19 L8 12 L13 12 Z" fill="white" stroke="black" strokeWidth="1.2" strokeLinejoin="round" />
    </svg>
  );
}
