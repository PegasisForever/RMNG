import { describe, expect, test } from "bun:test";

import { canOpenWindows } from "./clientLayout";

describe("canOpenWindows", () => {
  const nav = (userAgent: string, platform: string, maxTouchPoints = 0) => ({ userAgent, platform, maxTouchPoints }) as Navigator;

  test("a desktop browser can", () => {
    expect(canOpenWindows(nav("Mozilla/5.0 (X11; Linux x86_64) Chrome/140.0", "Linux x86_64"))).toBe(true);
    expect(canOpenWindows(nav("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/605.1.15", "MacIntel"))).toBe(true);
    expect(canOpenWindows(nav("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/140.0", "Win32", 10))).toBe(true);
  });

  test("a phone or a tablet cannot, an iPad that says it is a Mac too", () => {
    expect(canOpenWindows(nav("Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) Mobile/15E148", "iPhone", 5))).toBe(false);
    expect(canOpenWindows(nav("Mozilla/5.0 (Linux; Android 15) Chrome/140.0 Mobile", "Linux armv8l", 5))).toBe(false);
    expect(canOpenWindows(nav("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/605.1.15", "MacIntel", 5))).toBe(false);
  });
});
