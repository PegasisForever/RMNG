// How many screen (device) pixels one pixel of the clone's monitor takes in the other
// orientations (`./FullDesk.tsx`): the size Fit gives the monitor, and the largest the screen is
// shown at. This phone's own setting, kept in its browser storage, not on the server.
import { PHONE_SCALE } from "./geometry";

/** The choices the setting offers. */
export const PIXEL_RATIOS = [1, 1.25, 1.5, 2, 2.5, 3] as const;

const KEY = "rmng.ng.pixelRatio";

/** The phone's setting, or PHONE_SCALE (the upright one) when it has none. */
export function readPixelRatio(): number {
  try {
    const v = Number(localStorage.getItem(KEY));
    return (PIXEL_RATIOS as readonly number[]).includes(v) ? v : PHONE_SCALE;
  } catch {
    return PHONE_SCALE;
  }
}

export function savePixelRatio(ratio: number) {
  try {
    localStorage.setItem(KEY, String(ratio));
  } catch {
    // no storage (a private window): the choice lasts until the page closes
  }
}
