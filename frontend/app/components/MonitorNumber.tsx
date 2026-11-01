/** A monitor window's number while the Edit layout dialog is open, so the dialog's boxes can be
 *  matched to the windows: a square of glass (`~/ng/GLASS.md`) in the top-left corner, always
 *  the same size. */
export function MonitorNumber({ monitor }: { monitor: number }) {
  return (
    <div
      className="ng-glass ng-glass-thick pointer-events-none absolute z-50 flex size-36 flex-col items-center justify-center rounded-2xl"
      style={{ top: "calc(env(safe-area-inset-top) + 8px)", left: "calc(env(safe-area-inset-left) + 8px)" }}
    >
      <span className="text-7xl font-bold leading-none tabular-nums">{monitor}</span>
      <span className="mt-2 text-sm font-medium text-black/55 dark:text-white/60">{monitor === 0 ? "Primary" : "Monitor"}</span>
    </div>
  );
}
