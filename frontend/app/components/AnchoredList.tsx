// An open dropdown's list, drawn on the body under the field it opened from (above it when there
// is more room there), and kept there while the page scrolls or resizes.
//
// On the body because a list inside the field's own box is drawn in that box's layers: under
// what is over the box, cut by a scrolling column, and, inside glass (a dialog, Settings), a
// glass piece in another one, which cannot blur through it (`~/desk/GLASS.md`, Rules for the
// code). On the body, the list is glass of its own over the field's dialog, and blurs it.
//
// One look for every open list (GLASS.md, "An open list"): thick glass over the clone's screen;
// in the control panel's own window (no screen under it, `useModalSurface`) a solid panel. Its
// rows are flat (`LIST_ROW`).
import { type ReactNode, useLayoutEffect, useState } from "react";
import { createPortal } from "react-dom";

import { useModalSurface } from "~/components/ModalShell";
import { FLOAT_SOLID, R_PIECE, TEXT, TEXT_3 } from "~/desk/control/ui";

/** The most room a list takes, CSS px (`max-h-64`), and its gap from the field. */
const LIST_MAX = 256;
const GAP = 4;
/** The least width of a list, CSS px: a small field's (a pixel scale's) is wider than it. */
const LIST_MIN_W = 96;

/** The list's surface, as the field's corners. Its content is flat (`.on-glass`: a slate part
 *  in a row reads on it). */
const LIST_SURFACE = {
  glass: `on-glass glass glass-thick overflow-hidden ${R_PIECE}`,
  solid: `on-glass overflow-hidden ${FLOAT_SOLID} ${R_PIECE}`,
};

/** The list in the surface: it scrolls, with no outline of its own when it has the focus. */
export const LIST = "max-h-64 overflow-y-auto overscroll-y-contain py-1 outline-none";
/** A row, its states: under the mouse or the arrow keys (`on`), the chosen one (with a check),
 *  one that cannot be picked, and a group's caption. */
export const LIST_ROW = {
  base: "flex cursor-pointer items-center gap-2 px-3 py-1.5 text-sm",
  on: "bg-black/5 active:bg-black/10 dark:bg-white/10 dark:active:bg-white/15",
  idle: TEXT,
  chosen: `font-medium ${TEXT}`,
  disabled: `cursor-default ${TEXT_3}`,
  header: `px-3 pb-0.5 pt-1.5 text-[10px] font-semibold uppercase tracking-wide ${TEXT_3}`,
};

type Place = { left: number; width: number; top?: number; bottom?: number };

/** An unsafe edge of the display (the clock bar, the home bar), CSS px; 0 where none is set. */
function inset(side: "top" | "bottom"): number {
  return parseFloat(getComputedStyle(document.documentElement).getPropertyValue(`--safe-${side}`)) || 0;
}

function placeFor(field: HTMLElement): Place {
  const r = field.getBoundingClientRect();
  const width = Math.max(r.width, LIST_MIN_W);
  // Under the field, from its left; kept inside the window when it is wider than the field
  const left = Math.max(GAP, Math.min(r.left, window.innerWidth - width - GAP));
  // The room in the safe area under the field, and over it
  const below = window.innerHeight - inset("bottom") - r.bottom;
  const up = below < LIST_MAX + GAP && r.top - inset("top") > below;
  return up
    ? { left, width, bottom: window.innerHeight - r.top + GAP }
    : { left, width, top: r.bottom + GAP };
}

export function AnchoredList({ anchor, children }: { anchor: HTMLElement | null; children: ReactNode }) {
  const surface = useModalSurface();
  const [place, setPlace] = useState<Place | null>(null);
  useLayoutEffect(() => {
    if (!anchor) return;
    const move = () => setPlace(placeFor(anchor));
    move();
    window.addEventListener("scroll", move, true);
    window.addEventListener("resize", move);
    return () => {
      window.removeEventListener("scroll", move, true);
      window.removeEventListener("resize", move);
    };
  }, [anchor]);
  if (!anchor || !place || typeof document === "undefined") return null;
  return createPortal(
    // Over the dialogs (`ModalShell`: z-50)
    <div className="fixed z-[70]" style={place}>
      <div className={LIST_SURFACE[surface]}>{children}</div>
    </div>,
    document.body,
  );
}
