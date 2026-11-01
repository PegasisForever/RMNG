// The key chord that shows and hides the /ng pages in mouse and keyboard mode
// (`./inputMode.ts`): Option+Space on macOS, Super+Space elsewhere. The system may keep it for
// itself (GNOME and Windows give Super+Space to the input source) and hand it to the page only
// in the page's own fullscreen, where the keys are locked (`~/viewer/input.ts`; not the
// browser's fullscreen): it needs to work there only. Its modifier is held back from the clone
// until the next key (`holds`): a Super or an Alt pressed and let go alone does something there
// (GNOME's overview, an app's menu bar).

export type Os = "mac" | "windows" | "linux";

export function currentOs(): Os {
  const p = typeof navigator === "undefined" ? "" : navigator.platform;
  return /Mac/.test(p) ? "mac" : /Win/.test(p) ? "windows" : "linux";
}

type Chord = Pick<KeyboardEvent, "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">;

/** Whether `e` is the pages' chord on `os`. */
export function isPagesChord(e: Chord, os: Os = currentOs()): boolean {
  if (e.code !== "Space" || e.ctrlKey || e.shiftKey) return false;
  return os === "mac" ? e.altKey && !e.metaKey : e.metaKey && !e.altKey;
}

/** The keys (DOM codes) of the chord's modifier on `os`. */
export function pagesChordHolds(os: Os = currentOs()): readonly string[] {
  return os === "mac" ? ["AltLeft", "AltRight"] : ["MetaLeft", "MetaRight"];
}

/** The chord as the operator presses it on `os`. */
export function pagesChordLabel(os: Os = currentOs()): string {
  return os === "mac" ? "⌥ Space" : "Super Space";
}
