// The material everything that floats over the board is made of.
//
// One definition, because a second copy is how a dashboard ends up with two kinds of glass
// that almost match. The side panel's cards, a card in the air mid-drag, and the ⋮ menus all
// use it: they are all the same thing, a surface over the board rather than in it.
//
// Opacity and blur pull apart here rather than trade against each other. The fill is low so
// the columns show through, and the blur is what keeps text on top legible over them.
// Raising the fill is what kills it: at 70% the surface reads as a plain white panel,
// because the board is nearly white to begin with.

/** Fill and blur. Pair it with an outline and a shadow. */
export const GLASS_FILL = "bg-white/25 backdrop-blur-sm dark:bg-slate-900/25";

/** The same material, denser, for a menu. A menu is small text in tight rows that the
 *  operator reads once and dismisses, over whatever part of the board happens to be behind
 *  it. The light fill is right for a panel with room to breathe and wrong here: the rows
 *  need to separate from the board faster than they need to show it through. The heavier
 *  blur does the rest of that work. */
export const GLASS_FILL_DENSE = "bg-white/80 backdrop-blur-md dark:bg-slate-900/80";

/** The hairline every floating surface carries, so they are all outlined the one way. */
export const GLASS_OUTLINE = "border border-slate-900/10 dark:border-white/10";

/** Resting float: the side panel's cards, which sit still on the board's right edge.
 *
 *  Wide and faint rather than tight and dark, so the surface sits on the board instead of
 *  being cut out of it. Dark mode keeps that geometry but runs it in black at roughly five
 *  times the alpha. A shadow only shows as the difference between it and the surface under
 *  it, and against a slate-950 board the light-mode values come out to nothing. */
export const GLASS_SHADOW =
  "shadow-[0_2px_16px_rgb(15_23_42_/_0.05),0_10px_50px_rgb(15_23_42_/_0.07)] dark:shadow-[0_2px_16px_rgb(0_0_0_/_0.26),0_10px_50px_rgb(0_0_0_/_0.35)]";

/** Lifted: a dragged card, and a menu that just opened. Both are above the thing that
 *  spawned them, and how far the shadow spreads is what says so. */
export const GLASS_SHADOW_LIFTED =
  "shadow-[0_4px_16px_rgb(15_23_42_/_0.06),0_16px_48px_rgb(15_23_42_/_0.10)] dark:shadow-[0_4px_16px_rgb(0_0_0_/_0.3),0_16px_48px_rgb(0_0_0_/_0.4)]";

/** A row's hover and a divider inside glass. Both are alpha on whatever shows through: an
 *  opaque slate fill would punch a solid block into the surface and stop it reading as one. */
export const GLASS_HOVER = "hover:bg-slate-900/5 dark:hover:bg-white/10";
export const GLASS_DIVIDER = "bg-slate-900/10 dark:bg-white/10";
