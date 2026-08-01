// The clone-source image the operator last actually cloned from, remembered in localStorage.
//
// It lives here rather than in the picker because it is a session read, and a leaf that
// reads it on mount is not a function of its props: the picker would preselect a different
// image in Storybook than in the app, and the effect that does it fires `onChange` at the
// form. The clone dialog's container reads this on open and writes it back on a successful
// start, and the picker is handed the answer.
import type { ImageInfo } from "~/lib/wire/ImageInfo";

/** localStorage key holding the last image reference the operator cloned from. */
const LAST_IMAGE_KEY = "rmng.lastCloneImage";

/** Remember the image a clone was actually started from. Called by the dialog on submit,
 *  not on mere selection — a dropdown someone scrolled past shouldn't become the default. */
export function rememberCloneImage(reference: string): void {
  try {
    localStorage.setItem(LAST_IMAGE_KEY, reference);
  } catch {
    // Private mode / storage disabled — the picker just falls back to newest-created.
  }
}

/** The remembered reference, or null when there is none (or storage is unavailable). */
export function lastCloneImage(): string | null {
  try {
    return localStorage.getItem(LAST_IMAGE_KEY);
  } catch {
    return null;
  }
}

/** Which image a freshly opened dialog should start on: the last-cloned-from one if it still
 *  exists, else the newest-created. Null when there is nothing to pick from.
 *
 *  The existence check matters — an image can be deleted between clones, and a stale
 *  remembered reference would otherwise leave the dialog with nothing selected. */
export function preferredCloneImage(
  images: ImageInfo[],
  remembered: string | null,
): string | null {
  if (images.length === 0) return null;
  if (remembered && images.some((i) => i.reference === remembered)) return remembered;
  return images.reduce((newest, img) =>
    new Date(img.createdAt).getTime() > new Date(newest.createdAt).getTime() ? img : newest,
  ).reference;
}
