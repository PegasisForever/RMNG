// The clone dialog's two field classes, shared by its sections.
//
// They live in their own module rather than on CloneModalView because every section imports
// them and CloneModalView imports every section: exporting them from there would make that
// cycle load-bearing.

/** An input, select or textarea inside the dialog. */
export const cloneField =
 "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500";

/** The caption above a field. Also the class the `<label>` itself carries, so its text is
 *  small and muted while the field inside it stays normal weight. */
export const cloneLabel =
 "block text-xs font-medium text-slate-500 dark:text-slate-400";

/* Right-panel rows: caption in a fixed left column, control on the right. Phones fall back
   to caption-above (the old look) — a 10rem caption would leave no room for input. */
/** A field row: caption left, control right. */
export const cloneRow =
 "grid grid-cols-1 gap-1 sm:grid-cols-[10rem_minmax(0,1fr)] sm:items-center sm:gap-3";
/** Top-aligned variant for tall controls (textareas, the ticket editor). */
export const cloneRowTop =
 "grid grid-cols-1 gap-1 sm:grid-cols-[10rem_minmax(0,1fr)] sm:items-start sm:gap-3";
/** The caption in the left column. */
export const cloneRowLabel =
 "text-xs font-medium text-slate-500 sm:pt-0 dark:text-slate-400";
/** The caption in the left column of a top-aligned row: pushed down to the first text line. */
export const cloneRowLabelTop =
 "text-xs font-medium text-slate-500 sm:pt-2 dark:text-slate-400";
/** Control in the right column: `cloneField` without the top margin (the row sets spacing). */
export const cloneRowField =
 "w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500";

/** A right-panel section caption: what the block below it is. Bold and dark enough to
 *  stand apart from the field captions, still clearly a caption and not a control. */
export const cloneSectionCaption =
 "text-[11px] font-bold uppercase tracking-wider text-slate-600 dark:text-slate-400";
