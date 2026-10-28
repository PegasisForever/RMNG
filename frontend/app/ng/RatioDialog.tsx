// The dialog a long press on Fit opens (`./FloatingButtons.tsx`): how many screen pixels one
// clone pixel takes, with the monitor size each choice gives this display. A choice is saved on
// this phone (`./pixelRatio.ts`) and fits the clone to it at once.
import { X } from "lucide-react";
import { useRef } from "react";

import { phoneMonitorSize } from "./geometry";
import { PIXEL_RATIOS } from "./pixelRatio";

export function RatioDialog({
  ratio,
  width,
  height,
  dpr,
  onChoose,
  onClose,
}: {
  /** The setting now. */
  ratio: number;
  /** The display, CSS px, and its device px per CSS px. */
  width: number;
  height: number;
  dpr: number;
  onChoose: (ratio: number) => void;
  onClose: () => void;
}) {
  // A tap beside the dialog closes it, but only one that began there: the long press that opened
  // it ends in a click on the same spot, which is the backdrop by then
  const armed = useRef(false);
  return (
    <div
      className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onPointerDown={(e) => (armed.current = e.target === e.currentTarget)}
      onClick={(e) => {
        if (armed.current && e.target === e.currentTarget) onClose();
        armed.current = false;
      }}
    >
      <div
        role="dialog"
        aria-label="Clone pixel size"
        className="w-full max-w-md rounded-xl border border-neutral-700 bg-neutral-900 p-3 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2">
          <h2 className="flex-1 text-sm font-semibold text-neutral-100">Clone pixel size</h2>
          <button
            type="button"
            aria-label="Close"
            onClick={onClose}
            className="flex h-8 w-8 items-center justify-center rounded-md text-neutral-400 active:bg-neutral-800"
          >
            <X aria-hidden className="size-4" />
          </button>
        </div>
        <p className="mb-3 mt-1 text-xs text-neutral-400">
          How many screen pixels, across and down, one pixel of the clone takes. A smaller number gives the clone
          more room, with smaller text. Saved on this phone only.
        </p>
        <div className="grid grid-cols-3 gap-1.5">
          {PIXEL_RATIOS.map((r) => {
            const size = phoneMonitorSize(width, height, dpr, r);
            return (
              <button
                key={r}
                type="button"
                aria-pressed={r === ratio}
                onClick={() => onChoose(r)}
                className={`flex h-14 flex-col items-center justify-center rounded-lg text-sm ${
                  r === ratio ? "bg-neutral-100 text-black" : "bg-neutral-800 text-neutral-100 active:bg-neutral-700"
                }`}
              >
                <span className="font-semibold">{r}×</span>
                <span className={`font-mono text-[11px] ${r === ratio ? "text-neutral-600" : "text-neutral-400"}`}>
                  {size.width} × {size.height}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
