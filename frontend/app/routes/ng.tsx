// `/ng`: the installed phone app (Add to Home Screen; `public/manifest.webmanifest` opens it
// here). It is made for one device, the folding iPhone, unfolded and held upright, and shows
// nothing on any other screen. The selected clone's screen is on top, the keyboard and the
// mouse controls are below it: `~/ng/Desk.tsx`. The update rules are in `~/ng/pwa.ts`.
import { useEffect, useRef, useState } from "react";

import { Desk } from "~/ng/Desk";
import { attachPwa } from "~/ng/pwa";
import { isTyping } from "~/ng/systemKeyboard";

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

type Page = { size: [number, number]; bottom: number };

function measure(probe: HTMLElement): Page {
  return {
    size: [window.innerWidth, window.innerHeight],
    bottom: Math.round(parseFloat(getComputedStyle(probe).paddingBottom) || 0),
  };
}

/** Whether the page is the unfolded screen held upright. */
const fits = ([w, h]: [number, number]) => w === SCREEN.width && PAGE_HEIGHTS.includes(h);

export default function Ng() {
  const probe = useRef<HTMLDivElement>(null);
  const [page, setPage] = useState<Page | null>(null);
  // Null until the effect has looked: the page has no `window` while it is built
  const [secure, setSecure] = useState<boolean | null>(null);

  useEffect(() => {
    setSecure(window.isSecureContext);
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

    // The phone's keyboard can make the page shorter: while it is up the page stays as it was
    const update = () => probe.current && !isTyping() && setPage(measure(probe.current));
    update();
    // iOS can settle the viewport of a first open after the page loaded, with no resize event
    const settle = [100, 500, 1500].map((ms) => setTimeout(update, ms));
    window.addEventListener("resize", update);
    window.addEventListener("orientationchange", update);
    window.visualViewport?.addEventListener("resize", update);
    // and is measured again once the keyboard went down
    const onFocusOut = () => setTimeout(update, 600);
    document.addEventListener("focusout", onFocusOut);
    const detach = attachPwa();
    return () => {
      detach();
      settle.forEach(clearTimeout);
      window.removeEventListener("resize", update);
      window.removeEventListener("orientationchange", update);
      window.visualViewport?.removeEventListener("resize", update);
      document.removeEventListener("focusout", onFocusOut);
      if (meta && before !== undefined) meta.content = before;
      html.style.background = body.style.background = "";
      html.style.minHeight = "";
      body.style.overflow = "";
    };
  }, []);

  const ok = page !== null && fits(page.size);
  const message =
    secure === false
      ? "This page is not HTTPS: open it at the server's HTTPS address."
      : page && !ok
        ? "Unfold the phone and hold it upright."
        : null;

  return (
    // Sized to the screen, not to the page: the page can be 21 px short (PAGE_HEIGHTS)
    <div
      className="fixed left-0 top-0 touch-none select-none overflow-hidden bg-black text-white"
      style={ok ? SCREEN : { width: "100%", height: "100%" }}
    >
      <div ref={probe} className="pointer-events-none absolute inset-0 pb-[env(safe-area-inset-bottom)]" />
      {message && page ? (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-8 text-center text-slate-500">
          <div>{message}</div>
          <div className="font-mono text-sm">
            page {page.size[0]} × {page.size[1]}
          </div>
        </div>
      ) : null}
      {ok && secure ? (
        <Desk width={SCREEN.width} height={SCREEN.height} bottom={page.bottom} />
      ) : null}
    </div>
  );
}
