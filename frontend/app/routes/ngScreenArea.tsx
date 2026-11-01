// `/ng/screen-area`: a test page for the /ng app's screen, opened from the control panel's
// Settings ("Screen area"). It takes the page as /ng does (`~/ng/page.ts`) and draws two boxes:
// red, the whole area the page can draw in; green, the safe area (`env(safe-area-inset-*)`), the
// part clear of the clock bar, the home bar, the round corners and the camera's cut-out. Their
// numbers, and what the browser says of the window and the screen, are in the middle.
//
// It opens in the browser's own window, not in /ng's frame (`public/ng-shell.html`): a frame has
// no safe area of its own.
import { type ReactNode, useEffect, useRef, useState } from "react";

import { takePage } from "~/ng/page";

export function meta() {
  return [{ title: "RMNG screen area" }];
}

type Box = { x: number; y: number; w: number; h: number };

type Numbers = {
  /** The red box: where the page can draw. */
  page: Box;
  /** The green box: the safe area. */
  safe: Box;
  /** The safe area's insets: top, right, bottom, left. */
  insets: [number, number, number, number];
  inner: [number, number];
  /** The part of the page in view: width, height, its top. */
  visual: [number, number, number] | null;
  screen: [number, number];
  /** 100lvh, 100svh, 100dvh. */
  vh: [number, number, number];
  dpr: number;
  display: string;
  orientation: string;
};

const boxOf = (e: Element): Box => {
  const r = e.getBoundingClientRect();
  return { x: r.x, y: r.y, w: r.width, h: r.height };
};

/** The display mode the page is shown in: the installed app's (fullscreen, standalone) or a tab's. */
function displayMode(): string {
  return ["fullscreen", "standalone", "minimal-ui"].find((m) => matchMedia(`(display-mode: ${m})`).matches) ?? "browser";
}

const n = (v: number) => (Number.isInteger(v) ? String(v) : v.toFixed(1));
const size = (w: number, h: number) => `${n(w)} × ${n(h)}`;
const at = (b: Box) => `${size(b.w, b.h)} at ${n(b.x)}, ${n(b.y)}`;

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-white/50">{label}</dt>
      <dd>{children}</dd>
    </>
  );
}

export default function NgScreenArea() {
  const page = useRef<HTMLDivElement>(null);
  const safe = useRef<HTMLDivElement>(null);
  const vh = useRef<HTMLDivElement>(null);
  const [numbers, setNumbers] = useState<Numbers | null>(null);

  useEffect(() => {
    // The installed app's safety net (`root.tsx`) waits for an /ng page to say it shows
    window.__ngReady = true;
    const givePage = takePage();
    const measure = () => {
      if (!page.current || !safe.current || !vh.current) return;
      const s = getComputedStyle(safe.current);
      const [lvh, svh, dvh] = [...vh.current.children].map((c) => c.getBoundingClientRect().height);
      const v = window.visualViewport;
      setNumbers({
        page: boxOf(page.current),
        safe: boxOf(safe.current),
        insets: [s.top, s.right, s.bottom, s.left].map((p) => parseFloat(p) || 0) as Numbers["insets"],
        inner: [window.innerWidth, window.innerHeight],
        visual: v ? [v.width, v.height, v.offsetTop] : null,
        screen: [screen.width, screen.height],
        vh: [lvh, svh, dvh],
        dpr: window.devicePixelRatio,
        display: displayMode(),
        orientation: screen.orientation?.type ?? "",
      });
    };
    measure();
    // iOS can settle the page some time after it loads, with no event: look again each half second
    const timer = setInterval(measure, 500);
    window.addEventListener("resize", measure);
    window.visualViewport?.addEventListener("resize", measure);
    return () => {
      clearInterval(timer);
      window.removeEventListener("resize", measure);
      window.visualViewport?.removeEventListener("resize", measure);
      givePage();
    };
  }, []);

  return (
    <div className="fixed inset-0 select-text bg-black font-mono text-xs text-white">
      {/* The page's whole area; the border is inside the box, so its outer edge is the area's */}
      <div ref={page} className="pointer-events-none fixed inset-0 border-2 border-red-500">
        <span className="absolute left-1 top-1 text-red-400">page</span>
        <span className="absolute bottom-1 right-1 text-red-400">page</span>
      </div>
      <div
        ref={safe}
        className="pointer-events-none fixed border-2 border-emerald-400"
        style={{
          top: "env(safe-area-inset-top)",
          right: "env(safe-area-inset-right)",
          bottom: "env(safe-area-inset-bottom)",
          left: "env(safe-area-inset-left)",
        }}
      >
        <span className="absolute left-1 top-1 text-emerald-300">safe area</span>
        <span className="absolute bottom-1 right-1 text-emerald-300">safe area</span>
      </div>
      {/* Probes for the three viewport heights */}
      <div ref={vh} aria-hidden className="pointer-events-none invisible fixed left-0 top-0 flex">
        <div className="h-lvh w-px" />
        <div className="h-svh w-px" />
        <div className="h-dvh w-px" />
      </div>

      <div className="absolute inset-0 flex items-center justify-center p-8">
        <div className="max-w-full rounded-xl bg-white/10 p-4">
          {numbers ? (
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
              <Row label="page (red)">
                <span className="text-red-400">{at(numbers.page)}</span>
              </Row>
              <Row label="safe area (green)">
                <span className="text-emerald-300">{at(numbers.safe)}</span>
              </Row>
              <Row label="safe insets">
                top {n(numbers.insets[0])}, right {n(numbers.insets[1])}, bottom {n(numbers.insets[2])}, left{" "}
                {n(numbers.insets[3])}
              </Row>
              <Row label="window inner">{size(...numbers.inner)}</Row>
              <Row label="visual viewport">
                {numbers.visual ? `${size(numbers.visual[0], numbers.visual[1])}, top ${n(numbers.visual[2])}` : "none"}
              </Row>
              <Row label="screen">{size(...numbers.screen)}</Row>
              <Row label="lvh / svh / dvh">{numbers.vh.map(n).join(" / ")}</Row>
              <Row label="pixel ratio">{n(numbers.dpr)}</Row>
              <Row label="display mode">{numbers.display}</Row>
              <Row label="orientation">{numbers.orientation}</Row>
            </dl>
          ) : (
            "Measuring…"
          )}
          <button
            type="button"
            onClick={() => location.assign("/ng")}
            className="mt-4 w-full rounded-lg bg-white/15 px-3 py-2 font-sans text-sm active:bg-white/25"
          >
            Back to RMNG
          </button>
        </div>
      </div>
    </div>
  );
}
