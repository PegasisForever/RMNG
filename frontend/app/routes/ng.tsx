// `/ng`: the installed phone app (Add to Home Screen; `public/manifest.webmanifest` opens it
// here). It is made for one device, the folding iPhone, unfolded and held upright, and shows
// nothing on any other screen. A line the user drags divides the clone screen (above) from
// the keyboard and controls (below): `~/ng/SplitView.tsx`. The update rules are in
// `~/ng/pwa.ts`.
import { useEffect, useRef, useState } from "react";

import { SplitView } from "~/ng/SplitView";
import { attachPwa } from "~/ng/pwa";
import { type Bounds, clampSplit, defaultSplit } from "~/ng/split";

export function meta() {
  return [{ title: "RMNG" }];
}

/** The unfolded screen, held upright, in CSS px (a screenshot is 2007 × 2853 at 3×). */
const SCREEN = { width: 669, height: 951 };

/** The page size iOS gives the installed app on that screen: 21 px short at the bottom until
 *  the document is as tall as the screen (see the effect below), then the whole screen. Folded
 *  the page is 466 × 657; unfolded and flat, 951 × 648. */
const PAGE_HEIGHTS = [930, SCREEN.height];

/** The page runs under the clock bar and the home bar, and cannot be zoomed. */
const VIEWPORT =
  "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover";

/** Where the line was left, kept on this phone only. */
const SPLIT_KEY = "ng-split";

type Page = { size: [number, number]; top: number; bottom: number };

function measure(probe: HTMLElement): Page {
  const css = getComputedStyle(probe);
  const px = (v: string) => Math.round(parseFloat(v) || 0);
  return {
    size: [window.innerWidth, window.innerHeight],
    top: px(css.paddingTop),
    bottom: px(css.paddingBottom),
  };
}

/** Whether the page is the unfolded screen held upright. */
const fits = ([w, h]: [number, number]) => w === SCREEN.width && PAGE_HEIGHTS.includes(h);

function savedSplit(b: Bounds): number {
  try {
    const y = Number(localStorage.getItem(SPLIT_KEY));
    if (y > 0) return clampSplit(y, b);
  } catch {
    // no storage: start from the default
  }
  return defaultSplit(b);
}

function saveSplit(y: number) {
  try {
    localStorage.setItem(SPLIT_KEY, String(y));
  } catch {
    // no storage: the line goes back to the default next time
  }
}

export default function Ng() {
  const probe = useRef<HTMLDivElement>(null);
  const [page, setPage] = useState<Page | null>(null);
  const [split, setSplit] = useState<number | null>(null);

  useEffect(() => {
    const meta = document.querySelector<HTMLMetaElement>('meta[name="viewport"]');
    const before = meta?.content;
    if (meta) meta.content = VIEWPORT;
    // Black under the page too: iOS shows the strip below the page size
    const { documentElement: html, body } = document;
    html.style.background = body.style.background = "#000";
    body.style.overflow = "hidden";
    // iOS 26+ gives an installed app a page shorter than the screen while the document is
    // shorter than the screen, and paints nothing below that edge (WebKit bug 301108; the fix
    // from frappe-ui#1284). A document as tall as the screen gets the whole screen.
    html.style.minHeight = "min(100lvh, calc(100% + env(safe-area-inset-top)))";

    const update = () => probe.current && setPage(measure(probe.current));
    update();
    window.addEventListener("resize", update);
    window.addEventListener("orientationchange", update);
    const detach = attachPwa();
    return () => {
      detach();
      window.removeEventListener("resize", update);
      window.removeEventListener("orientationchange", update);
      if (meta && before !== undefined) meta.content = before;
      html.style.background = body.style.background = "";
      html.style.minHeight = "";
      body.style.overflow = "";
    };
  }, []);

  const ok = page !== null && fits(page.size);
  const bounds: Bounds | null = page && { height: SCREEN.height, top: page.top, bottom: page.bottom };
  // The safe area is known only after the first measure, so the line is placed then
  useEffect(() => {
    if (ok && page && split === null)
      setSplit(savedSplit({ height: SCREEN.height, top: page.top, bottom: page.bottom }));
  }, [ok, page, split]);

  return (
    // Sized to the screen, not to the page: the page can be 21 px short (PAGE_HEIGHTS)
    <div
      className="fixed left-0 top-0 touch-none select-none overflow-hidden bg-black text-white"
      style={ok ? SCREEN : { width: "100%", height: "100%" }}
    >
      <div
        ref={probe}
        className="pointer-events-none absolute inset-0 pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)]"
      />
      {page && !ok ? (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-8 text-center text-slate-500">
          <div>Unfold the phone and hold it upright.</div>
          <div className="font-mono text-sm">
            page {page.size[0]} × {page.size[1]}
          </div>
        </div>
      ) : null}
      {ok && bounds && split !== null ? (
        <SplitView
          width={SCREEN.width}
          {...bounds}
          split={split}
          onSplit={setSplit}
          onSplitEnd={(y) => {
            setSplit(y);
            saveSplit(y);
          }}
        />
      ) : null}
    </div>
  );
}
