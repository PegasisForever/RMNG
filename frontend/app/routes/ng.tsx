// `/ng`: the installed phone app (Add to Home Screen; `public/manifest.webmanifest` opens it
// here). It is made for one device, the folding iPhone, unfolded and held upright, and shows
// nothing on any other screen. For now it has no function: it shows the sizes it gets, so
// the layout can be made for them. The update rules are in `~/ng/pwa.ts`.
import { useEffect, useRef, useState } from "react";

import { attachPwa } from "~/ng/pwa";

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

type Insets = { top: number; right: number; bottom: number; left: number };

type Sizes = {
  inner: [number, number];
  visual: [number, number] | null;
  /** The heights of 100lvh, 100svh and 100dvh. */
  units: [number, number, number];
  insets: Insets;
  display: string;
  fullscreenApi: boolean;
};

function displayMode(): string {
  for (const mode of ["fullscreen", "standalone", "minimal-ui"])
    if (window.matchMedia(`(display-mode: ${mode})`).matches) return mode;
  // iOS reports the home-screen app here, not always through the media query
  return (navigator as { standalone?: boolean }).standalone ? "standalone (iOS)" : "browser";
}

type Webkit = {
  webkitFullscreenEnabled?: boolean;
  webkitRequestFullscreen?: () => Promise<void> | void;
};

function measure(probe: HTMLElement, units: HTMLElement): Sizes {
  const css = getComputedStyle(probe);
  const px = (v: string) => Math.round(parseFloat(v) || 0);
  const vv = window.visualViewport;
  const [lvh, svh, dvh] = Array.from(units.children, (c) => (c as HTMLElement).offsetHeight);
  return {
    inner: [window.innerWidth, window.innerHeight],
    visual: vv ? [Math.round(vv.width), Math.round(vv.height)] : null,
    units: [lvh ?? 0, svh ?? 0, dvh ?? 0],
    insets: {
      top: px(css.paddingTop),
      right: px(css.paddingRight),
      bottom: px(css.paddingBottom),
      left: px(css.paddingLeft),
    },
    display: displayMode(),
    fullscreenApi: Boolean(
      document.fullscreenEnabled || (document as Document & Webkit).webkitFullscreenEnabled,
    ),
  };
}

/** Whether the page is the unfolded screen held upright. */
const fits = ([w, h]: [number, number]) => w === SCREEN.width && PAGE_HEIGHTS.includes(h);

/** Asks for the Fullscreen API, to learn if it hides the clock bar; resolves to the outcome. */
async function tryFullscreen(): Promise<string> {
  const el = document.documentElement as HTMLElement & Webkit;
  const request = el.requestFullscreen?.bind(el) ?? el.webkitRequestFullscreen?.bind(el);
  if (!request) return "no Fullscreen API";
  try {
    await request();
    return document.fullscreenElement ? "on" : "asked, no change";
  } catch (e) {
    return `refused: ${e instanceof Error ? e.message : String(e)}`;
  }
}

const size = ([w, h]: [number, number]) => `${w} × ${h}`;

export default function Ng() {
  const probe = useRef<HTMLDivElement>(null);
  const units = useRef<HTMLDivElement>(null);
  const [sizes, setSizes] = useState<Sizes | null>(null);
  const [fullscreen, setFullscreen] = useState<string | null>(null);

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

    const update = () =>
      probe.current && units.current && setSizes(measure(probe.current, units.current));
    update();
    window.addEventListener("resize", update);
    window.addEventListener("orientationchange", update);
    window.visualViewport?.addEventListener("resize", update);
    document.addEventListener("fullscreenchange", update);
    const detach = attachPwa();
    return () => {
      detach();
      window.removeEventListener("resize", update);
      window.removeEventListener("orientationchange", update);
      window.visualViewport?.removeEventListener("resize", update);
      document.removeEventListener("fullscreenchange", update);
      if (meta && before !== undefined) meta.content = before;
      html.style.background = body.style.background = "";
      html.style.minHeight = "";
      body.style.overflow = "";
    };
  }, []);

  const ok = sizes !== null && fits(sizes.inner);
  const safe: [number, number] | null = sizes && [
    SCREEN.width - sizes.insets.left - sizes.insets.right,
    SCREEN.height - sizes.insets.top - sizes.insets.bottom,
  ];

  return (
    // The frame is the whole screen by its known size, not by the page size, so it reaches
    // the bottom. The red line is its edge; the green dashed line is the safe area inside it.
    <div
      className={`fixed left-0 top-0 touch-none select-none overflow-hidden bg-black text-white ${ok ? "outline outline-2 -outline-offset-2 outline-red-500" : "h-full w-full"}`}
      style={ok ? { width: SCREEN.width, height: SCREEN.height } : undefined}
    >
      <div ref={units} className="pointer-events-none invisible absolute left-0 top-0 flex">
        <div className="h-lvh w-px" />
        <div className="h-svh w-px" />
        <div className="h-dvh w-px" />
      </div>
      <div
        ref={probe}
        className="pointer-events-none absolute inset-0 pt-[env(safe-area-inset-top)] pr-[env(safe-area-inset-right)] pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)]"
      >
        {ok ? <div className="h-full w-full border-2 border-dashed border-green-500" /> : null}
      </div>
      {sizes && !ok ? (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-8 text-center text-slate-500">
          <div>Unfold the phone and hold it upright.</div>
          <div className="font-mono text-sm">page {size(sizes.inner)}</div>
        </div>
      ) : null}
      {sizes && ok && safe ? (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-6 p-8 text-center">
          <div className="text-3xl font-semibold text-green-400">Unfolded, vertical</div>
          <div>
            <div className="text-sm text-slate-400">Screen (CSS px)</div>
            <div className="font-mono text-6xl font-bold">{size([SCREEN.width, SCREEN.height])}</div>
          </div>
          <dl className="grid grid-cols-[auto_auto] gap-x-6 gap-y-1 text-left font-mono text-lg">
            <dt className="text-slate-400">Inside safe area</dt>
            <dd>{size(safe)}</dd>
            <dt className="text-slate-400">Safe area T R B L</dt>
            <dd>
              {sizes.insets.top} {sizes.insets.right} {sizes.insets.bottom} {sizes.insets.left}
            </dd>
            <dt className="text-slate-400">Page</dt>
            <dd>{size(sizes.inner)}</dd>
            <dt className="text-slate-400">Visual viewport</dt>
            <dd>{sizes.visual ? size(sizes.visual) : "none"}</dd>
            <dt className="text-slate-400">lvh svh dvh</dt>
            <dd>{sizes.units.join(" ")}</dd>
            <dt className="text-slate-400">Display mode</dt>
            <dd>{sizes.display}</dd>
            <dt className="text-slate-400">Fullscreen API</dt>
            <dd>{sizes.fullscreenApi ? "yes" : "no"}</dd>
          </dl>
          <button
            type="button"
            className="rounded-lg border border-slate-600 px-4 py-2 text-lg"
            onClick={() => void tryFullscreen().then(setFullscreen)}
          >
            Try to hide the clock bar
          </button>
          {fullscreen ? <div className="font-mono text-sm text-slate-400">{fullscreen}</div> : null}
        </div>
      ) : null}
    </div>
  );
}
