// The clone dialog's two field classes, shared by its sections.
//
// They live in their own module rather than on CloneModalView because every section imports
// them and CloneModalView imports every section: exporting them from there would make that
// cycle load-bearing.
//
// Flat content on the dialog's panel (`~/desk/GLASS.md`): the shared field, and the grey texts.
import { FIELD, TEXT_2 } from "~/desk/control/ui";

/** A field's text: normal weight in a label, and its hint in the least text's grey. */
const FIELD_TEXT = "px-3 py-2 text-sm font-normal placeholder:text-black/40 dark:placeholder:text-white/40";

/** An input, select or textarea inside the dialog. */
export const cloneField = `mt-1 w-full ${FIELD} ${FIELD_TEXT}`;

/** The caption above a field. Also the class the `<label>` itself carries, so its text is
 *  small and muted while the field inside it stays normal weight. */
export const cloneLabel = `block text-xs font-medium ${TEXT_2}`;

/* Right-panel rows: caption in a fixed left column, control on the right. Phones fall back
   to caption-above (the old look) — a 10rem caption would leave no room for input. */
/** A field row: caption left, control right. */
export const cloneRow =
 "grid grid-cols-1 gap-1 sm:grid-cols-[10rem_minmax(0,1fr)] sm:items-center sm:gap-3";
/** Top-aligned variant for tall controls (textareas, the ticket editor). */
export const cloneRowTop =
 "grid grid-cols-1 gap-1 sm:grid-cols-[10rem_minmax(0,1fr)] sm:items-start sm:gap-3";
/** The caption in the left column. */
export const cloneRowLabel = `text-xs font-medium sm:pt-0 ${TEXT_2}`;
/** The caption in the left column of a top-aligned row: pushed down to the first text line. */
export const cloneRowLabelTop = `text-xs font-medium sm:pt-2 ${TEXT_2}`;
/** Control in the right column: `cloneField` without the top margin (the row sets spacing). */
export const cloneRowField = `w-full ${FIELD} ${FIELD_TEXT}`;
/** A box round a rich editor (a ticket's description) that looks as a field: its line goes dark
 *  while the editor in it has the focus. */
export const cloneEditorBox =
 "rounded-[10px] border border-black/10 bg-white/60 px-3 py-2 text-sm font-normal text-neutral-900 focus-within:border-black/30 dark:border-white/10 dark:bg-black/30 dark:text-white dark:focus-within:border-white/30";

/** A right-panel section caption: what the block below it is. Bold and dark enough to
 *  stand apart from the field captions, still clearly a caption and not a control. */
export const cloneSectionCaption = `text-[11px] font-bold uppercase tracking-wider ${TEXT_2}`;
