// The browser's own fullscreen (F11, the green button on a Mac, the View menu): the window fills
// the screen, but the page's fullscreen is off, so the keys are not locked. Esc, the system's
// chords and the control panel's chord then stay with the browser or the system instead of
// reaching the clone (`./keys.ts`). Only the page's own fullscreen (RMNG's Fullscreen button)
// locks them, so the screen asks the operator to switch (`./BrowserFullscreenDialog.tsx`).

import type { Os } from "./keys";

/** What this reads of the browser window (the shell's, `~/lib/host.ts`); a test gives a fake. */
export interface FullscreenWindow {
  /** The window's size with the browser's frame: CSS px that the page's zoom does not change. */
  outerWidth: number;
  outerHeight: number;
  screen: { width: number; height: number };
  document: { fullscreenElement: Element | null; fullscreenEnabled: boolean };
  navigator: { userAgent: string; maxTouchPoints: number };
  matchMedia(query: string): { matches: boolean };
}

const DISPLAY_FULLSCREEN = "(display-mode: fullscreen)";

/** Safari on a Mac (not an iPad, which says it is a Mac but has touch points). */
function macSafari(n: FullscreenWindow["navigator"]): boolean {
  return (
    /Macintosh/.test(n.userAgent) &&
    /Safari\//.test(n.userAgent) &&
    !/Chrome|Chromium|Edg\//.test(n.userAgent) &&
    n.maxTouchPoints === 0
  );
}

/** The window is in the browser's own fullscreen, with the page's fullscreen off. A browser
 *  with no page fullscreen (an iPhone) has nothing better to offer, so never. Chrome, Edge and
 *  Firefox say so by the display mode. Safari on a Mac only by the window's size: there no
 *  window that is not fullscreen covers the menu bar. Only there, as elsewhere a maximized window
 *  can cover the whole screen (a taskbar that hides itself). */
export function inBrowserFullscreen(w: FullscreenWindow): boolean {
  const doc = w.document;
  if (!doc.fullscreenEnabled || doc.fullscreenElement) return false;
  if (w.matchMedia(DISPLAY_FULLSCREEN).matches) return true;
  return (
    macSafari(w.navigator) &&
    w.outerWidth >= w.screen.width &&
    w.outerHeight >= w.screen.height
  );
}

/** Call `f` when the browser's fullscreen may have come or gone. Returns the stop. */
export function watchBrowserFullscreen(w: Window, f: () => void): () => void {
  const media = w.matchMedia(DISPLAY_FULLSCREEN);
  media.addEventListener("change", f);
  w.addEventListener("resize", f);
  w.document.addEventListener("fullscreenchange", f);
  return () => {
    media.removeEventListener("change", f);
    w.removeEventListener("resize", f);
    w.document.removeEventListener("fullscreenchange", f);
  };
}

/** The keys that leave the browser's fullscreen on `os`. */
export function leaveBrowserFullscreenKeys(os: Os): string {
  return os === "mac" ? "⌃ Control + ⌘ Command + F" : "F11";
}
