// An open dropdown's list, drawn on the body under the field it opened from (above it when there
// is more room there), and kept there while the page scrolls or resizes.
//
// On the body because a list inside the field's own box is drawn in that box's layers: under
// what is over the box, cut by a scrolling column, and, inside /ng's glass (a dialog, Settings),
// a blurred layer in another, whose blur Safari (and Chrome on some machines) does not draw. A
// list from inside the glass keeps its look (`.ng-on-glass`, `~/ng/glass.css`).
import { type ReactNode, useLayoutEffect, useState } from "react";
import { createPortal } from "react-dom";

/** The most room a list takes, CSS px (`max-h-64`), and its gap from the field. */
const LIST_MAX = 256;
const GAP = 4;

type Place = { left: number; width: number; top?: number; bottom?: number };

function placeFor(field: HTMLElement): Place {
  const r = field.getBoundingClientRect();
  const below = window.innerHeight - r.bottom;
  const up = below < LIST_MAX + GAP && r.top > below;
  return up
    ? { left: r.left, width: r.width, bottom: window.innerHeight - r.top + GAP }
    : { left: r.left, width: r.width, top: r.bottom + GAP };
}

export function AnchoredList({ anchor, children }: { anchor: HTMLElement | null; children: ReactNode }) {
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
  const glass = anchor.closest(".ng-on-glass") ? "ng-on-glass" : "";
  return createPortal(
    // Over the dialogs (`ModalShell`: z-50, z-60)
    <div className={`fixed z-[70] ${glass}`} style={place}>
      {children}
    </div>,
    document.body,
  );
}
