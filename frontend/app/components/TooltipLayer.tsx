// The app's tooltips, in glass (`~/desk/GLASS.md`): an element with `data-tip` (and, for a
// second line in grey, `data-tip-sub`) gets one when the mouse rests on it for TIP_DELAY_MS.
// One layer for the whole page draws it, on the body, so it is never glass inside glass (a glass
// piece cannot blur through the glass it is in), and an element only names its text. A finger
// gets no tooltip: a touch screen has no hover.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

/** How long the mouse rests on an element before its tooltip comes (GLASS.md, Motion). */
const TIP_DELAY_MS = 300;
/** Space between the element and its tooltip, and the tooltip and the window's edges, CSS px. */
const GAP = 6;

type Tip = { text: string; sub?: string; box: DOMRect };

export function TooltipLayer() {
  const [tip, setTip] = useState<Tip | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let el: HTMLElement | null = null;
    const hide = () => {
      clearTimeout(timer);
      el = null;
      setTip(null);
    };
    const over = (e: PointerEvent) => {
      if (e.pointerType !== "mouse") return;
      const t = e.target instanceof Element ? e.target.closest<HTMLElement>("[data-tip]") : null;
      if (t === el) return;
      hide();
      if (!t?.dataset.tip) return;
      el = t;
      timer = setTimeout(() => {
        if (!el?.isConnected || !el.dataset.tip) return;
        setTip({ text: el.dataset.tip, sub: el.dataset.tipSub, box: el.getBoundingClientRect() });
      }, TIP_DELAY_MS);
    };
    const leave = (e: PointerEvent) => {
      if (!e.relatedTarget) hide();
    };
    document.addEventListener("pointerover", over, true);
    document.addEventListener("pointerout", leave, true);
    document.addEventListener("pointerdown", hide, true);
    document.addEventListener("keydown", hide, true);
    document.addEventListener("scroll", hide, true);
    window.addEventListener("blur", hide);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("pointerover", over, true);
      document.removeEventListener("pointerout", leave, true);
      document.removeEventListener("pointerdown", hide, true);
      document.removeEventListener("keydown", hide, true);
      document.removeEventListener("scroll", hide, true);
      window.removeEventListener("blur", hide);
    };
  }, []);

  // Under the element, in its middle; above it where there is no room below; always inside
  // the window
  useLayoutEffect(() => {
    const t = ref.current;
    if (!tip || !t) return setAt(null);
    const { width, height } = t.getBoundingClientRect();
    const { box } = tip;
    const below = box.bottom + GAP + height + GAP <= window.innerHeight;
    const left = Math.min(Math.max(box.left + box.width / 2 - width / 2, GAP), window.innerWidth - width - GAP);
    setAt({ left, top: below ? box.bottom + GAP : box.top - GAP - height });
  }, [tip]);

  if (!tip || typeof document === "undefined") return null;
  return createPortal(
    <div
      ref={ref}
      role="tooltip"
      className={`glass glass-thick pointer-events-none fixed z-[70] flex max-w-xs flex-col rounded-lg px-2.5 py-1.5 text-xs text-neutral-900 transition-opacity duration-150 dark:text-white ${
        at ? "opacity-100" : "opacity-0"
      }`}
      style={at ?? { left: 0, top: 0 }}
    >
      <span>{tip.text}</span>
      {tip.sub ? <span className="text-black/60 dark:text-white/60">{tip.sub}</span> : null}
    </div>,
    document.body,
  );
}
