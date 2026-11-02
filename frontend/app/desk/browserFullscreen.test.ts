import { describe, expect, test } from "bun:test";

import {
  type FullscreenWindow,
  inBrowserFullscreen,
  leaveBrowserFullscreenKeys,
} from "./browserFullscreen";

const CHROME_MAC =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const SAFARI_MAC =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

/** A 1920×1080 screen; the window `w`×`h` with its frame; the display mode, the page's
 *  fullscreen and the browser as given. */
function win(
  o: {
    w?: number;
    h?: number;
    mode?: boolean;
    page?: boolean;
    enabled?: boolean;
    ua?: string;
    touch?: number;
  } = {},
): FullscreenWindow {
  return {
    outerWidth: o.w ?? 1920,
    outerHeight: o.h ?? 1080,
    screen: { width: 1920, height: 1080 },
    document: {
      fullscreenElement: o.page ? ({} as Element) : null,
      fullscreenEnabled: o.enabled ?? true,
    },
    navigator: { userAgent: o.ua ?? CHROME_MAC, maxTouchPoints: o.touch ?? 0 },
    matchMedia: () => ({ matches: o.mode ?? false }),
  };
}

describe("inBrowserFullscreen", () => {
  test("the browser's fullscreen, by the display mode (Chrome, Edge, Firefox)", () => {
    expect(inBrowserFullscreen(win({ mode: true }))).toBe(true);
  });
  test("Safari on a Mac: by the window's size, which covers the menu bar", () => {
    expect(inBrowserFullscreen(win({ ua: SAFARI_MAC }))).toBe(true);
    expect(inBrowserFullscreen(win({ ua: SAFARI_MAC, h: 1055 }))).toBe(false);
  });
  test("not by the size elsewhere: a maximized window can cover the screen", () => {
    expect(inBrowserFullscreen(win({ mode: false }))).toBe(false);
    // An iPad says it is a Mac, but it has touch points
    expect(inBrowserFullscreen(win({ ua: SAFARI_MAC, touch: 5 }))).toBe(false);
  });
  test("not in the page's own fullscreen, which locks the keys", () => {
    expect(inBrowserFullscreen(win({ mode: true, page: true }))).toBe(false);
    expect(inBrowserFullscreen(win({ ua: SAFARI_MAC, page: true }))).toBe(
      false,
    );
  });
  test("never where the page has no fullscreen to offer (an iPhone)", () => {
    expect(inBrowserFullscreen(win({ mode: true, enabled: false }))).toBe(
      false,
    );
  });
});

test("the keys that leave the browser's fullscreen", () => {
  expect(leaveBrowserFullscreenKeys("mac")).toBe("⌃ Control + ⌘ Command + F");
  expect(leaveBrowserFullscreenKeys("windows")).toBe("F11");
  expect(leaveBrowserFullscreenKeys("linux")).toBe("F11");
});
