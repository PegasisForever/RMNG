import { type PointerEvent, useRef } from "react";

type Props = {
  /** The keyboard area's height, CSS px; the screen has the rest of `total`. */
  value: number;
  min: number;
  max: number;
  total: number;
  /** The height the page uses without a choice made here. */
  standard: number;
  onChange: (value: number) => void;
  onClose: () => void;
};

/** For now: a dialog to try other heights for the keyboard area. The keys stay square and
 *  get smaller where the area is too short for them, and the screen letterboxes the clone's
 *  monitor in what is left. The slider is drawn here (not an <input type="range">), so it
 *  works the same with the page's touch handling off. */
export function SplitDialog({ value, min, max, total, standard, onChange, onClose }: Props) {
  const track = useRef<HTMLDivElement>(null);
  const part = (value - min) / (max - min);
  const at = (e: PointerEvent) => {
    const r = track.current!.getBoundingClientRect();
    const t = Math.min(Math.max((e.clientX - r.left) / r.width, 0), 1);
    onChange(Math.round(min + t * (max - min)));
  };
  return (
    <div className="absolute inset-0 z-20 flex items-center justify-center bg-black/60" onPointerDown={onClose}>
      <div
        role="dialog"
        aria-label="Keyboard height"
        className="w-[480px] space-y-5 rounded-2xl border border-neutral-800 bg-neutral-900 p-5 text-neutral-100"
        onPointerDown={(e) => e.stopPropagation()}
      >
        <div className="text-lg font-medium">Keyboard height</div>
        <div
          ref={track}
          role="slider"
          aria-valuemin={min}
          aria-valuemax={max}
          aria-valuenow={value}
          className="relative h-10"
          onPointerDown={(e) => {
            e.currentTarget.setPointerCapture(e.pointerId);
            at(e);
          }}
          onPointerMove={(e) => {
            if (e.currentTarget.hasPointerCapture(e.pointerId)) at(e);
          }}
        >
          <div className="absolute inset-x-0 top-1/2 h-1.5 -translate-y-1/2 rounded-full bg-neutral-700" />
          <div
            className="absolute top-1/2 h-1.5 -translate-y-1/2 rounded-full bg-neutral-300"
            style={{ width: `${part * 100}%` }}
          />
          <div
            className="absolute top-1/2 h-7 w-7 -translate-x-1/2 -translate-y-1/2 rounded-full bg-white"
            style={{ left: `${part * 100}%` }}
          />
        </div>
        <div className="font-mono text-sm text-neutral-400">
          keyboard {value} px · screen {total - value} px
        </div>
        <div className="flex justify-end gap-3">
          <button
            type="button"
            className="rounded-lg bg-neutral-800 px-4 py-2"
            onPointerDown={() => onChange(standard)}
          >
            Default ({standard})
          </button>
          <button type="button" className="rounded-lg bg-neutral-200 px-4 py-2 text-black" onPointerDown={onClose}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
