// The material the ⋮ menus are made of, and the hairline every card-shaped thing carries.
//
// One definition, because a second copy is how an app ends up with two kinds of glass that
// almost match.

/** Fill and blur, for a menu. A menu is small text in tight rows that the operator reads once
 *  and dismisses, over whatever happens to be behind it, so the rows need to separate from it
 *  fast: the fill is dense and the blur heavy. Pair it with an outline and a shadow. */
export const GLASS_FILL_DENSE = "bg-white/80 backdrop-blur-md dark:bg-slate-900/80";

/** The hairline every floating surface and card carries, so they are all outlined the one way. */
export const GLASS_OUTLINE = "border border-slate-900/10 dark:border-white/10";

/** Lifted: a menu that just opened. It is above the thing that spawned it, and how far the
 *  shadow spreads is what says so.
 *
 *  Dark mode keeps the geometry but runs it in black at a higher alpha. A shadow only shows as
 *  the difference between it and the surface under it, and against a dark surface the
 *  light-mode values come out to nothing. */
export const GLASS_SHADOW_LIFTED =
  "shadow-[0_4px_16px_rgb(15_23_42_/_0.06),0_16px_48px_rgb(15_23_42_/_0.10)] dark:shadow-[0_4px_16px_rgb(0_0_0_/_0.3),0_16px_48px_rgb(0_0_0_/_0.4)]";

/** A row's hover and a divider inside glass. Both are alpha on whatever shows through: an
 *  opaque slate fill would punch a solid block into the surface and stop it reading as one. */
export const GLASS_HOVER = "hover:bg-slate-900/5 dark:hover:bg-white/10";
export const GLASS_DIVIDER = "bg-slate-900/10 dark:bg-white/10";
