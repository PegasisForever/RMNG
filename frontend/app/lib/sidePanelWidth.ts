// How wide the dashboard's side panel was left, remembered in localStorage.
//
// It lives here rather than in the shell because it is a session read, and a shell that
// reads it on mount is not a function of its props: the panel would open at whatever width
// the last drag left, which is a different panel in Storybook than in the app and reviewable
// in neither. The dashboard's container reads the stored width after mount and writes the
// new one back when a drag settles; the shell is handed a starting width and reports out.

/** localStorage key holding the side panel's width, as a whole percentage. */
const SIDE_KEY = "rmng.sidePanelWidth";

/** The panel's width, as a percentage of the shell. The board pads its own right edge by the
 *  same number, so the two can never disagree and leave the last column stranded under the
 *  cards — which is why the panel has no pixel floor of its own. */
export const SIDE_MIN = 20;
export const SIDE_MAX = 60;
export const SIDE_DEFAULT = 30;

/** Hold a width inside the range the panel stays usable across. */
export function clampSideWidth(pct: number): number {
  return Math.min(SIDE_MAX, Math.max(SIDE_MIN, pct));
}

/** Remember the width a drag, an arrow-key nudge or a double-click reset settled on. */
export function rememberSideWidth(pct: number): void {
  try {
    localStorage.setItem(SIDE_KEY, String(Math.round(pct)));
  } catch {
    // Private mode / storage disabled — the panel opens at the default next time.
  }
}

/** The remembered width, clamped, or null when there is none (or storage is unavailable).
 *  A stored `0` reads as none: the panel cannot be that narrow, so the value is junk. */
export function storedSideWidth(): number | null {
  try {
    const stored = Number(localStorage.getItem(SIDE_KEY));
    if (!Number.isFinite(stored) || stored <= 0) return null;
    return clampSideWidth(stored);
  } catch {
    return null;
  }
}
