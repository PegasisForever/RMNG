// The clone dialog's form model, as a story hands it over.
//
// The base is the real initial state (`emptyCloneDraft`) with a source image already picked,
// because that is what the operator sees a beat after the dialog opens: the picker chooses
// the last-cloned-from image on its own, and nothing else is filled in. Layer a tab and its
// fields on top of that.

import { emptyCloneDraft, type CloneDraft } from "~/lib/cloneDraft";

import { makeImage } from "./images";

/** The image reference the picker settles on, matching the base image fixture. */
export const cloneImage: string = makeImage().reference;

export function makeCloneDraft(overrides: Partial<CloneDraft> = {}): CloneDraft {
  return { ...emptyCloneDraft(), image: cloneImage, ...overrides };
}

/** A Linear link of the shape a dragged ticket card seeds the field with. The parser reads an
 *  id out of this exactly as it would out of a bare `WE-142`. */
export const cloneTicketUrl = "https://linear.app/pegasis/issue/WE-142/normalize-sidebar-cpu";
