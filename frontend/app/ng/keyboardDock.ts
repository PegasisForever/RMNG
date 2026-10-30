// Whether the /ng keyboard is docked (`./FullDesk.tsx`): always shown under the clone's screen,
// which is then a touch screen. A long press on the keyboard button changes it. This phone's own
// setting, kept in its browser storage, not on the server.
const KEY = "rmng.ng.keyboardDocked";

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
