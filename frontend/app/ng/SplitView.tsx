import { type PointerEvent, useRef } from "react";

import { type Bounds, clampSplit } from "./split";

/** The touch area of the line, taller than the line looks, half over each area. */
const GRIP = 36;

type Props = Bounds & {
  width: number;
  /** The line's distance from the top of the screen. */
  split: number;
  /** The line moves under a finger, already clamped. */
  onSplit: (y: number) => void;
  /** The finger lifts: the line stays here. */
  onSplitEnd: (y: number) => void;
};

/** The /ng screen in two: the clone screen above the line, the keyboard and the controls
 *  below it. Both are empty boxes for now, with their sizes. Drag the line to move it. The
 *  screen area starts under the clock bar (`top`); the keyboard area runs to the bottom and
 *  keeps the home bar (`bottom`) clear inside it. */
export function SplitView({ width, height, top, bottom, split, onSplit, onSplitEnd }: Props) {
  /** The finger's distance from the line when it took hold, so the line does not jump. */
  const grab = useRef<number | null>(null);
  const bounds = { height, top, bottom };
  const at = (e: PointerEvent) => clampSplit(e.clientY - (grab.current ?? 0), bounds);

  const down = (e: PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    grab.current = e.clientY - split;
  };
  const move = (e: PointerEvent) => {
    if (grab.current !== null) onSplit(at(e));
  };
  const up = (e: PointerEvent) => {
    if (grab.current === null) return;
    const y = at(e);
    grab.current = null;
    onSplitEnd(y);
  };

  const screen = split - top;
  const keys = height - split - bottom;
  return (
    <div className="relative bg-black text-slate-400" style={{ width, height }}>
      <div
        className="absolute inset-x-0 flex items-center justify-center bg-slate-900"
        style={{ top, height: screen }}
      >
        <Label name="Screen" size={`${width} × ${screen}`} />
      </div>
      <div
        className="absolute inset-x-0 flex items-center justify-center bg-slate-950"
        style={{ top: split, height: height - split, paddingBottom: bottom }}
      >
        <Label name="Keyboard + controls" size={`${width} × ${keys}`} />
      </div>
      <div
        role="separator"
        aria-orientation="horizontal"
        aria-valuenow={split}
        className="absolute inset-x-0 flex cursor-row-resize touch-none items-center justify-center"
        style={{ top: split - GRIP / 2, height: GRIP }}
        onPointerDown={down}
        onPointerMove={move}
        onPointerUp={up}
        onPointerCancel={up}
      >
        <div className="absolute inset-x-0 h-px bg-slate-600" />
        <div className="relative h-1.5 w-16 rounded-full bg-slate-400" />
      </div>
    </div>
  );
}

function Label({ name, size }: { name: string; size: string }) {
  return (
    <div className="text-center">
      <div className="text-lg">{name}</div>
      <div className="font-mono text-sm text-slate-500">{size}</div>
    </div>
  );
}
