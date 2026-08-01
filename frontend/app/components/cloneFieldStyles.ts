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
export const cloneLabel = "block text-xs font-medium text-slate-500 dark:text-slate-400";
