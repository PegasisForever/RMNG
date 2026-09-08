// The clone dialog's form model, as a story hands it over.
//
// The base is the real initial state (`emptyCloneDraft`) with a source clone already
// picked, because that is what the operator sees a beat after the dialog opens: the picker
// chooses the first forkable clone on its own. The new clone id is derived server-side
// from the ticket or title, so no id field exists. Layer a tab and its fields on top of that.

import { emptyCloneDraft, type CloneDraft } from "~/lib/cloneDraft";

/** The source clone id the picker settles on, matching the first clone fixture. */
export const cloneSource: string = "pega-we-142";

export function makeCloneDraft(overrides: Partial<CloneDraft> = {}): CloneDraft {
  return { ...emptyCloneDraft(), source: cloneSource, ...overrides };
}

/** A Linear link of the shape a dragged ticket card seeds the field with. The parser reads an
 *  id out of this exactly as it would out of a bare `WE-142`. */
export const cloneTicketUrl = "https://linear.app/pegasis/issue/WE-142/normalize-sidebar-cpu";
