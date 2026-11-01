// Where a window that /ng opens goes so that it leaves this page's fullscreen alone. Chrome takes
// a page out of its fullscreen when the page opens a window on the same screen (a page must not
// put a window of its own over its fullscreen); a window on another screen leaves it as it is.
// To place a window on another screen the page needs the Window Management API and its
// permission, which Chrome asks for the first time. Without them (Firefox, Safari, a "Block"),
// or with one screen, there is no other screen, and the window opens where the browser puts it.

/** A screen as the Window Management API gives it, in the screens' coordinates (CSS px, from
 *  the primary screen's top-left corner). */
export type ScreenBox = {
  left: number;
  top: number;
  availLeft: number;
  availTop: number;
  availWidth: number;
  availHeight: number;
  isPrimary: boolean;
};

declare global {
  interface Window {
    getScreenDetails?: () => Promise<{ screens: ScreenBox[]; currentScreen: ScreenBox }>;
  }
}

export type WindowBox = { left: number; top: number; width: number; height: number };

/** The screen to use instead of `here`: the primary one, or else the first other; null when
 *  there is no other. */
export function otherScreen(screens: readonly ScreenBox[], here: ScreenBox): ScreenBox | null {
  const others = screens.filter((s) => s !== here && (s.left !== here.left || s.top !== here.top));
  return others.find((s) => s.isPrimary) ?? others[0] ?? null;
}

/** A window `width`×`height` in the middle of `screen`'s free area, made smaller to fit it. */
export function centeredOn(screen: ScreenBox, width: number, height: number): WindowBox {
  const w = Math.min(width, screen.availWidth);
  const h = Math.min(height, screen.availHeight);
  return {
    left: Math.round(screen.availLeft + (screen.availWidth - w) / 2),
    top: Math.round(screen.availTop + (screen.availHeight - h) / 2),
    width: w,
    height: h,
  };
}

/** A window `width`×`height` in the middle of a screen other than this window's; null when there
 *  is none, or the browser cannot say (no API, permission refused). The first time, Chrome asks
 *  for the permission: the click that asked may then be too old to open a window. */
export async function boxOnOtherScreen(width: number, height: number): Promise<WindowBox | null> {
  if (!window.getScreenDetails) return null;
  try {
    const { screens, currentScreen } = await window.getScreenDetails();
    const screen = otherScreen(screens, currentScreen);
    return screen ? centeredOn(screen, width, height) : null;
  } catch {
    return null;
  }
}
