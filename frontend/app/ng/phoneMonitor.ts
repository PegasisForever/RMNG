// The /ng page's layout (`~/lib/clientLayout.ts`): one monitor, its size this phone's own,
// kept in its browser storage. Fit sets it to the screen's area (`./FullDesk.tsx`); before the
// first Fit it follows the screen's area.
const KEY = "rmng.ng.monitor";

type Size = { width: number; height: number };

/** The size Fit set last; null before the first Fit. */
export function readPhoneMonitor(): Size | null {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "null") as Size | null;
    return v && v.width > 0 && v.height > 0 ? { width: v.width, height: v.height } : null;
  } catch {
    return null;
  }
}

export function savePhoneMonitor(size: Size) {
  try {
    localStorage.setItem(KEY, JSON.stringify(size));
  } catch {
    // no storage (a private window): the size lasts until the page closes
  }
}
