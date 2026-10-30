// The /ng keyboard's settings (`./FullDesk.tsx`), this phone's own, kept in its browser storage,
// not on the server: whether it is docked (always shown under the clone's screen, which is then
// a touch screen; a long press on the keyboard button changes it), and its height (the bar on
// top of it, `./KeyboardHandle.tsx`).
const KEY = "rmng.ng.keyboardDocked";
const HEIGHT_KEY = "rmng.ng.keyboardHeight";

export function readKeyboardDocked(): boolean {
  try {
    return localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

export function saveKeyboardDocked(docked: boolean) {
  try {
    localStorage.setItem(KEY, docked ? "1" : "0");
  } catch {
    // no storage (a private window): the choice lasts until the page closes
  }
}

/** The keyboard's height as a part of the display's height; null before it was set. */
export function readKeyboardHeight(): number | null {
  try {
    const v = Number(localStorage.getItem(HEIGHT_KEY));
    return v > 0 && v < 1 ? v : null;
  } catch {
    return null;
  }
}

export function saveKeyboardHeight(part: number) {
  try {
    localStorage.setItem(HEIGHT_KEY, String(part));
  } catch {
    // no storage (a private window): the height lasts until the page closes
  }
}
