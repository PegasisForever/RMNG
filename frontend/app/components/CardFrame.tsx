// The frame a card wears where it sits on a solid surface (a dialog's running job): rounded,
// outlined, opaque, and shadowed to sit on it.
import type { ReactNode } from "react";

import { GLASS_OUTLINE } from "~/lib/glass";

/** A card sits close to the surface under it, so its shadow is tighter than a floating
 *  panel's.
 *
 *  Dark mode runs the same geometry at roughly five times the alpha, in black rather than
 *  slate. A shadow is only visible as the difference between it and the surface under it,
 *  and against a dark surface the light-mode values come out to nothing. */
const RESTING =
  "shadow-[0_1px_6px_rgb(15_23_42_/_0.04),0_4px_16px_rgb(15_23_42_/_0.05)] dark:shadow-[0_1px_6px_rgb(0_0_0_/_0.22),0_4px_16px_rgb(0_0_0_/_0.26)]";

/** The frame: the same hairline the floating panels carry (`GLASS_OUTLINE`), so everything
 *  card-shaped on screen is outlined the one way. Nothing in it is selectable: its text is a
 *  label, not content anybody copies out by hand. */
export function CardFrame({ children }: { children: ReactNode }) {
  return (
    <div
      className={`select-none overflow-hidden rounded-lg bg-white dark:bg-slate-900 ${GLASS_OUTLINE} ${RESTING}`}
    >
      {children}
    </div>
  );
}
