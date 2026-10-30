// The key chord that opens and closes the /ng pages in mouse and keyboard mode
// (`./inputMode.ts`). Super+Space; but the system keeps it for itself (GNOME and Windows: the
// input source; macOS: Command+Space, Spotlight) and hands it to the page only in the page's
// own fullscreen, where the keys are locked (`~/viewer/input.ts`; not the browser's fullscreen).
// So each system also gets a chord that it leaves alone: Ctrl+Alt+Space on Linux and Windows,
// Command+Shift+Space on macOS (where Ctrl+Option+Space is the input source).

export type Os = "mac" | "windows" | "linux";

export function currentOs(): Os {
  const p = typeof navigator === "undefined" ? "" : navigator.platform;
  return /Mac/.test(p) ? "mac" : /Win/.test(p) ? "windows" : "linux";
}

type Chord = Pick<KeyboardEvent, "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">;

/** Whether `e` is the pages' chord on `os`. */
export function isPagesChord(e: Chord, os: Os = currentOs()): boolean {
  if (e.code !== "Space") return false;
  if (e.metaKey && !e.ctrlKey && !e.altKey) return true;
  return os !== "mac" && e.ctrlKey && e.altKey && !e.metaKey && !e.shiftKey;
}

/** The chord as the operator presses it on `os`. */
export function pagesChordLabel(os: Os = currentOs()): string {
  return os === "mac" ? "⌘ ⇧ Space" : "Ctrl Alt Space";
}
