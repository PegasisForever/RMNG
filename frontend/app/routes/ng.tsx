// `/ng`: the installed phone app (Add to Home Screen; `public/manifest.webmanifest` opens it
// here). It is made for the folding iPhone, unfolded and held upright. For now it has no
// function: it shows the screen state it detects and the sizes it can use, so the layout can
// be made for those sizes. The update rules are in `~/ng/pwa.ts`.
import { useEffect, useRef, useState } from "react";

import { attachPwa } from "~/ng/pwa";

export function meta() {
  return [{ title: "RMNG" }];
}

/** Short side over long side above this is the unfolded screen: folded, an iPhone is near
 *  0.46; unfolded, the screen is close to square. Set from a guess; fix it from the sizes
 *  this page shows on the device. */
const UNFOLDED_RATIO = 0.6;

/** The page runs under the clock bar and the home bar, and cannot be zoomed. */
const VIEWPORT =
  "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover";

type Insets = { top: number; right: number; bottom: number; left: number };

type Sizes = {
  inner: [number, number];
  visual: [number, number] | null;
  client: [number, number];
  screen: [number, number];
  dpr: number;
  insets: Insets;
  display: string;
  orientation: string;
};

function displayMode(): string {
  for (const mode of ["fullscreen", "standalone", "minimal-ui"])
    if (window.matchMedia(`(display-mode: ${mode})`).matches) return mode;
  // iOS reports the home-screen app here, not always through the media query
  return (navigator as { standalone?: boolean }).standalone ? "standalone (iOS)" : "browser";
}

function measure(probe: HTMLElement): Sizes {
  const css = getComputedStyle(probe);
  const px = (v: string) => Math.round(parseFloat(v) || 0);
  const vv = window.visualViewport;
  return {
    inner: [window.innerWidth, window.innerHeight],
    visual: vv ? [Math.round(vv.width), Math.round(vv.height)] : null,
    client: [document.documentElement.clientWidth, document.documentElement.clientHeight],
    screen: [window.screen.width, window.screen.height],
    dpr: window.devicePixelRatio,
    insets: {
      top: px(css.paddingTop),
      right: px(css.paddingRight),
      bottom: px(css.paddingBottom),
      left: px(css.paddingLeft),
    },
    display: displayMode(),
    orientation: window.screen.orientation?.type ?? "unknown",
  };
}

/** What the screen is doing, from the shape of the page. */
function posture([w, h]: [number, number]): { label: string; ok: boolean } {
  const unfolded = Math.min(w, h) / Math.max(w, h) > UNFOLDED_RATIO;
  const upright = h >= w;
  if (unfolded && upright) return { label: "Unfolded, vertical", ok: true };
  if (unfolded) return { label: "Unfolded, horizontal: turn it upright", ok: false };
  return { label: "Folded: unfold the phone", ok: false };
}

const size = ([w, h]: [number, number]) => `${w} × ${h}`;

export default function Ng() {
  const probe = useRef<HTMLDivElement>(null);
  const [sizes, setSizes] = useState<Sizes | null>(null);

  useEffect(() => {
    const meta = document.querySelector<HTMLMetaElement>('meta[name="viewport"]');
    const before = meta?.content;
    if (meta) meta.content = VIEWPORT;
    const html = document.documentElement;
    html.style.background = "#000";
    document.body.style.overflow = "hidden";

    const update = () => probe.current && setSizes(measure(probe.current));
    update();
    window.addEventListener("resize", update);
    window.addEventListener("orientationchange", update);
    window.visualViewport?.addEventListener("resize", update);
    const detach = attachPwa();
    return () => {
      detach();
      window.removeEventListener("resize", update);
      window.removeEventListener("orientationchange", update);
      window.visualViewport?.removeEventListener("resize", update);
      if (meta && before !== undefined) meta.content = before;
      html.style.background = "";
      document.body.style.overflow = "";
    };
  }, []);

  const state = sizes && posture(sizes.inner);
  const safe: [number, number] | null = sizes && [
    sizes.inner[0] - sizes.insets.left - sizes.insets.right,
    sizes.inner[1] - sizes.insets.top - sizes.insets.bottom,
  ];

  return (
    // The red line is the edge of the page; the green dashed line is the safe area inside it.
    <div className="fixed inset-0 touch-none select-none overflow-hidden bg-black text-white outline outline-2 -outline-offset-2 outline-red-500">
      <div
        ref={probe}
        className="pointer-events-none absolute inset-0 pt-[env(safe-area-inset-top)] pr-[env(safe-area-inset-right)] pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)]"
      >
        <div className="h-full w-full border-2 border-dashed border-green-500" />
      </div>
      {sizes && state && safe ? (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-6 p-8 text-center">
          <div className={`text-3xl font-semibold ${state.ok ? "text-green-400" : "text-amber-400"}`}>
            {state.label}
          </div>
          <div>
            <div className="text-sm text-slate-400">Usable page size (CSS px)</div>
            <div className="font-mono text-6xl font-bold">{size(sizes.inner)}</div>
          </div>
          <dl className="grid grid-cols-[auto_auto] gap-x-6 gap-y-1 text-left font-mono text-lg">
            <dt className="text-slate-400">Inside safe area</dt>
            <dd>{size(safe)}</dd>
            <dt className="text-slate-400">Safe area T R B L</dt>
            <dd>
              {sizes.insets.top} {sizes.insets.right} {sizes.insets.bottom} {sizes.insets.left}
            </dd>
            <dt className="text-slate-400">Visual viewport</dt>
            <dd>{sizes.visual ? size(sizes.visual) : "none"}</dd>
            <dt className="text-slate-400">Document</dt>
            <dd>{size(sizes.client)}</dd>
            <dt className="text-slate-400">Screen</dt>
            <dd>{size(sizes.screen)}</dd>
            <dt className="text-slate-400">Pixel ratio</dt>
            <dd>{sizes.dpr}</dd>
            <dt className="text-slate-400">Display mode</dt>
            <dd>{sizes.display}</dd>
            <dt className="text-slate-400">Orientation</dt>
            <dd>{sizes.orientation}</dd>
          </dl>
        </div>
      ) : null}
    </div>
  );
}
