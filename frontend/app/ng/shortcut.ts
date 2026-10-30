// The key chord that opens and closes the /ng pages in mouse and keyboard mode
// (`./inputMode.ts`). Super+Space on Linux (GNOME gives it to the input source switch, which a
// clone used from the browser does not need). The other systems keep their own Super+Space
// (Windows: the input language; macOS: Command+Space, Spotlight) and never hand it to the page
// outside fullscreen, so each gets a chord of its own that the system leaves alone:
// Command+Shift+Space on macOS, Ctrl+Alt+Space on Windows. Super+Space counts everywhere: it
// reaches the page in fullscreen, where the keyboard is locked (`~/viewer/input.ts`).

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
  return os === "windows" && e.ctrlKey && e.altKey && !e.metaKey && !e.shiftKey;
}

/** The chord as the operator presses it on `os`. */
export function pagesChordLabel(os: Os = currentOs()): string {
  return os === "mac" ? "⌘ ⇧ Space" : os === "windows" ? "Ctrl Alt Space" : "Super Space";
}
