// The layout a browser gets the first time it opens the app (`~/lib/clientLayout.ts`): one
// monitor, this window's size.
import type { BrowserLayout, ClientKind } from "~/lib/clientLayout";

import { PHONE_SCALE } from "./geometry";
import { PHONE_SCROLL_SPEED } from "./pointer";

type Size = { width: number; height: number };

/** The pixel scale the app starts with: 2 on a phone, so text on the clone is as large as on the
 *  phone's own apps; 1 on a desktop. */
export const startScale = (kind: ClientKind) => (kind === "phone" ? PHONE_SCALE : 1);

/** The first layout: `fit(scale)` is the window's size at a pixel scale. */
export function firstLayout(kind: ClientKind, fit: (scale: number) => Size): BrowserLayout {
  const scale = startScale(kind);
  return {
    monitors: [{ ...fit(scale), x: 0, y: 0, primary: true }],
    scrollMode: "auto",
    scrollSpeed: kind === "phone" ? PHONE_SCROLL_SPEED : 100,
    scales: [scale],
  };
}
