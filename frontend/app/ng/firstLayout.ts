// The layout a browser gets the first time it opens /ng (`~/lib/clientLayout.ts`): one monitor,
// this window's size. A phone that kept its monitor and pixel ratio of its own before
// (`rmng.ng.monitor`, `rmng.ng.pixelRatio`, builds up to 2026-10-30) starts from those.
import type { BrowserLayout, ClientKind } from "~/lib/clientLayout";
import { PIXEL_SCALES } from "~/lib/clientLayout";

import { PHONE_SCALE } from "./geometry";
import { PHONE_SCROLL_SPEED } from "./pointer";

type Size = { width: number; height: number };

function stored(key: string): unknown {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "null");
  } catch {
    return null;
  }
}

/** The pixel scale /ng starts with: 2 on a phone, so text on the clone is as large as on the
 *  phone's own apps; 1 on a desktop. */
export const startScale = (kind: ClientKind) => (kind === "phone" ? PHONE_SCALE : 1);

/** The first layout: `fit(scale)` is the window's size at a pixel scale. */
export function firstLayout(kind: ClientKind, fit: (scale: number) => Size): BrowserLayout {
  const oldRatio = stored("rmng.ng.pixelRatio");
  const scale = (PIXEL_SCALES as readonly unknown[]).includes(oldRatio) ? (oldRatio as number) : startScale(kind);
  const old = stored("rmng.ng.monitor") as Size | null;
  const size = old && old.width > 0 && old.height > 0 ? { width: old.width, height: old.height } : fit(scale);
  return {
    monitors: [{ ...size, x: 0, y: 0, primary: true }],
    scrollMode: "auto",
    scrollSpeed: kind === "phone" ? PHONE_SCROLL_SPEED : 100,
    scales: [scale],
  };
}
