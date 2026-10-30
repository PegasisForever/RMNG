/** A monitor window's number, large in its middle, while the Edit layout dialog is open, so the
 *  dialog's boxes can be matched to the windows. */
export function MonitorNumber({ monitor }: { monitor: number }) {
  return (
    <div className="pointer-events-none absolute inset-0 z-50 flex items-center justify-center">
      <div className="flex flex-col items-center rounded-3xl bg-black/70 px-16 py-8 text-white shadow-2xl ring-4 ring-emerald-500 backdrop-blur-md">
        <span className="text-[28vmin] font-bold leading-none tabular-nums">{monitor}</span>
        <span className="mt-2 text-[4vmin] font-medium text-emerald-300">
          {monitor === 0 ? "Monitor 0 · primary" : `Monitor ${monitor}`}
        </span>
      </div>
    </div>
  );
}
