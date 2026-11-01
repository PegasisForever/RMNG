// A monitor window's number while the Edit layout dialog is open, so the dialog's boxes can be
// matched to the windows: a square of glass (`~/ng/GLASS.md`) in the top-left corner, always the
// same size. It comes in and goes out as a dialog does (`MODAL_ANIMATION`), and is drawn on the
// body over everything, dialogs and their backdrops too (the Edit layout dialog is one).
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

import { MODAL_ANIMATION } from "~/components/ModalShell";
import { MODAL_EXIT_MS } from "~/lib/useModalExit";

/** `monitor`: the number to show; null: none (the last one plays its way out first). */
export function MonitorNumber({ monitor }: { monitor: number | null }) {
  // The number on screen: the current one, or the last one while it goes out
  const [shown, setShown] = useState<number | null>(monitor);
  useEffect(() => {
    if (monitor !== null) return setShown(monitor);
    const t = window.setTimeout(() => setShown(null), MODAL_EXIT_MS);
    return () => window.clearTimeout(t);
  }, [monitor]);
  const n = monitor ?? shown;
  if (n === null || typeof document === "undefined") return null;
  return createPortal(
    <div
      aria-hidden={monitor === null}
      className={`ng-glass ng-glass-thick pointer-events-none fixed z-[80] flex size-36 flex-col items-center justify-center rounded-2xl ${
        monitor !== null ? MODAL_ANIMATION.panel.enter : MODAL_ANIMATION.panel.exit
      }`}
      style={{ top: "calc(env(safe-area-inset-top) + 8px)", left: "calc(env(safe-area-inset-left) + 8px)" }}
    >
      <span className="text-7xl font-bold leading-none tabular-nums">{n}</span>
      <span className="mt-2 text-sm font-medium text-black/55 dark:text-white/60">{n === 0 ? "Primary" : "Monitor"}</span>
    </div>,
    document.body,
  );
}
