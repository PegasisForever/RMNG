// The Edit layout dialog: this browser's monitors (`~/lib/clientLayout.ts`), as the clones get
// them while the operator uses this browser. Every change applies at once: there is no Save.
//
// - The arrangement: each monitor a box, moved only by drag and drop. While it is dragged it
//   snaps to touch another monitor's edge, lined up with its edges or its center when close
//   (`~/lib/monitorLayout.ts`); a dashed outline shows where the pointer is. In each box, three
//   buttons: make it monitor 0 (the primary one), give it the size of its viewer window (in
//   screen pixels divided by the pixel scale), and remove it (its viewer window closes).
//   Sizes come only from the windows: there is no field to type one in.
// - Add a monitor, at the right; a viewer window opens for it.
// - The pixel scale: screen pixels per clone pixel.
// - How the viewer reads the wheel, and the scroll speed.
//
// While the dialog is open, each viewer window shows its monitor's number large in its middle
// (the container tells them), so the boxes here can be matched to the windows.
//
// Pure: the layout comes in and every change goes out, with what it does to the monitors'
// numbers (`LayoutChange`). The container saves and applies it, opens and renumbers the viewer
// windows, and asks them for their sizes.
import { Maximize2, Plus, Star, Trash2 } from "lucide-react";
import { type PointerEvent, type ReactNode, useLayoutEffect, useRef, useState } from "react";

import { ModalShell } from "~/components/ModalShell";
import { settingsInput } from "~/components/SettingsFields";
import { type DesktopLayout, PIXEL_SCALES } from "~/lib/clientLayout";
import {
  MAX_MONITORS,
  addMonitor,
  dropMonitor,
  makePrimary,
  primaryMap,
  removeMap,
  removeMonitor,
  snapMonitor,
} from "~/lib/monitorLayout";
import type { MonitorSpec } from "~/lib/wire/MonitorSpec";
import type { ScrollMode } from "~/lib/wire/ScrollMode";

/** The arrangement's height, and the room around it, CSS px. */
const VIEW_H = 230;
const VIEW_PAD = 12;

const SCROLL_MODES: { value: ScrollMode; label: string }[] = [
  { value: "auto", label: "Auto (mouse or trackpad, per event)" },
  { value: "trackpad", label: "Trackpad" },
  { value: "mouse", label: "Mouse wheel" },
];

/** What a change does to the monitors' numbers: a monitor added (its number), or monitors
 *  renumbered (`map[old]` is the new number; null: removed). */
export type LayoutChange = { t: "add"; monitor: number } | { t: "renumber"; map: (number | null)[] };

export interface LayoutEditorViewProps {
  /** This browser's layout. */
  layout: DesktopLayout;
  /** A change, applied at once. */
  onChange: (layout: DesktopLayout, change?: LayoutChange) => void;
  /** Give monitor `i` the size of its viewer window. */
  onMatchWindow: (i: number) => void;
  /** The monitor whose window is being asked for its size; null when none is. */
  matching: number | null;
  /** A short note on the last match that did nothing (no window open); null for none. */
  note: string | null;
  /** The dialog is on screen; false plays the exit. */
  open?: boolean;
  /** The dialog has gone (after its exit). */
  onClose: () => void;
}

export function LayoutEditorView({
  layout,
  onChange,
  onMatchWindow,
  matching,
  note,
  open = true,
  onClose,
}: LayoutEditorViewProps) {
  const mons = layout.monitors;

  return (
    <ModalShell size="md" open={open} onExited={onClose}>
      {(close) => (
        <>
          <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">Edit layout</h3>
          <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
            This browser&apos;s monitors. The clones get them while you use this browser, and another device&apos;s
            while you use that one. Each viewer window shows its monitor&apos;s number while this is open.
          </p>

          <Arrangement
            monitors={mons}
            matching={matching}
            onDrop={(i, x, y) => onChange({ ...layout, monitors: dropMonitor(mons, i, x, y) })}
            onPrimary={(i) =>
              onChange({ ...layout, monitors: makePrimary(mons, i) }, { t: "renumber", map: primaryMap(mons.length, i) })
            }
            onMatchWindow={onMatchWindow}
            onRemove={(i) =>
              onChange({ ...layout, monitors: removeMonitor(mons, i) }, { t: "renumber", map: removeMap(mons.length, i) })
            }
          />
          {note ? <p className="mt-1 text-[11px] text-amber-600 dark:text-amber-400">{note}</p> : null}

          <div className="mt-3 flex items-center gap-2">
            <button
              type="button"
              disabled={mons.length >= MAX_MONITORS}
              onClick={() => onChange({ ...layout, monitors: addMonitor(mons) }, { t: "add", monitor: mons.length })}
              className="inline-flex items-center gap-1 rounded-md border border-slate-200 px-2.5 py-1 text-xs font-medium text-slate-700 hover:bg-slate-100 disabled:opacity-40 dark:border-slate-700 dark:text-slate-200 dark:hover:bg-slate-800"
            >
              <Plus aria-hidden className="size-3.5" />
              Add monitor
            </button>
            <label className="ml-auto flex items-center gap-1.5 whitespace-nowrap text-xs text-slate-500 dark:text-slate-400">
              Pixel scale
              <select
                aria-label="Pixel scale"
                value={layout.pixelScale}
                onChange={(e) => onChange({ ...layout, pixelScale: Number(e.target.value) })}
                className={settingsInput.replace("w-full", "w-20")}
              >
                {PIXEL_SCALES.map((s) => (
                  <option key={s} value={s}>
                    {s}×
                  </option>
                ))}
              </select>
            </label>
          </div>

          <div className="mt-3 grid grid-cols-[1fr_7rem] gap-2">
            <label className="block">
              <span className="mb-0.5 block text-xs font-medium text-slate-500 dark:text-slate-400">Scroll</span>
              <select
                value={layout.scrollMode}
                onChange={(e) => onChange({ ...layout, scrollMode: e.target.value as ScrollMode })}
                className={settingsInput}
              >
                {SCROLL_MODES.map((m) => (
                  <option key={m.value} value={m.value}>
                    {m.label}
                  </option>
                ))}
              </select>
            </label>
            <label className="block">
              <span className="mb-0.5 block text-xs font-medium text-slate-500 dark:text-slate-400">Speed %</span>
              <input
                type="number"
                step={5}
                value={layout.scrollSpeed}
                onChange={(e) => onChange({ ...layout, scrollSpeed: Number(e.target.value) || 0 })}
                className={settingsInput}
              />
            </label>
          </div>

          <div className="mt-4 flex items-center justify-end gap-2">
            <span className="mr-auto text-[11px] text-slate-400 dark:text-slate-500">Changes apply at once.</span>
            <button
              type="button"
              onClick={() => close()}
              className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700"
            >
              Done
            </button>
          </div>
        </>
      )}
    </ModalShell>
  );
}

/** The monitors drawn to scale, each with its three buttons. A drag moves one, snapped where it
 *  would land; the drop puts it there (`onDrop`, in monitor pixels). */
function Arrangement({
  monitors,
  matching,
  onDrop,
  onPrimary,
  onMatchWindow,
  onRemove,
}: {
  monitors: MonitorSpec[];
  matching: number | null;
  onDrop: (i: number, x: number, y: number) => void;
  onPrimary: (i: number) => void;
  onMatchWindow: (i: number) => void;
  onRemove: (i: number) => void;
}) {
  const area = useRef<HTMLDivElement>(null);
  const [areaW, setAreaW] = useState(460);
  useLayoutEffect(() => {
    if (area.current) setAreaW(area.current.clientWidth);
  }, []);
  // The drawing's scale and offset stay as they were while a drag is on, so the box under the
  // pointer does not run away from it
  const frame = (ms: MonitorSpec[]) => {
    const x0 = Math.min(...ms.map((m) => m.x));
    const y0 = Math.min(...ms.map((m) => m.y));
    const w = Math.max(...ms.map((m) => m.x + m.width)) - x0;
    const h = Math.max(...ms.map((m) => m.y + m.height)) - y0;
    // Room around it to drag into
    const scale = Math.min((areaW - 2 * VIEW_PAD) / (w * 1.4), (VIEW_H - 2 * VIEW_PAD) / (h * 1.4));
    return { scale, ox: (areaW - w * scale) / 2 - x0 * scale, oy: (VIEW_H - h * scale) / 2 - y0 * scale };
  };
  const [drag, setDrag] = useState<{ i: number; sx: number; sy: number; dx: number; dy: number } | null>(null);
  const kept = useRef(frame(monitors));
  if (!drag) kept.current = frame(monitors);
  const f = kept.current;

  const down = (i: number) => (e: PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    setDrag({ i, sx: e.clientX, sy: e.clientY, dx: 0, dy: 0 });
  };
  const move = (e: PointerEvent<HTMLDivElement>) => {
    if (drag) setDrag({ ...drag, dx: e.clientX - drag.sx, dy: e.clientY - drag.sy });
  };
  const up = () => {
    if (!drag) return;
    const m = monitors[drag.i];
    if (Math.hypot(drag.dx, drag.dy) > 3) onDrop(drag.i, m.x + drag.dx / f.scale, m.y + drag.dy / f.scale);
    setDrag(null);
  };

  return (
    <div
      ref={area}
      role="group"
      aria-label="Monitor arrangement"
      className="relative mt-3 w-full touch-none select-none rounded-md border border-slate-200 bg-slate-50 dark:border-slate-700 dark:bg-slate-900"
      style={{ height: VIEW_H }}
    >
      {monitors.map((m, i) => {
        const moving = drag?.i === i;
        const box = { width: m.width * f.scale, height: m.height * f.scale };
        // Dragged: drawn where it would land, the pointer's place dashed
        const raw = moving ? { x: m.x + drag.dx / f.scale, y: m.y + drag.dy / f.scale } : m;
        const at = (moving && snapMonitor(monitors, i, raw.x, raw.y)) || raw;
        const primary = i === 0;
        return (
          <div key={i}>
            {moving ? (
              <div
                aria-hidden
                className="pointer-events-none absolute rounded border border-dashed border-slate-400 dark:border-slate-500"
                style={{ left: f.ox + raw.x * f.scale, top: f.oy + raw.y * f.scale, ...box }}
              />
            ) : null}
            <div
              aria-label={`Monitor ${i}${primary ? " (primary)" : ""}`}
              className={`absolute flex cursor-grab flex-col items-center justify-center rounded border-[1.5px] ${
                moving ? "z-10 cursor-grabbing opacity-90" : ""
              } ${
                primary
                  ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950"
                  : "border-slate-400 bg-white dark:border-slate-600 dark:bg-slate-800"
              }`}
              style={{ left: f.ox + at.x * f.scale, top: f.oy + at.y * f.scale, ...box }}
              onPointerDown={down(i)}
              onPointerMove={move}
              onPointerUp={up}
              onPointerCancel={() => setDrag(null)}
            >
              <span className="text-xs font-semibold text-slate-700 dark:text-slate-100">
                {i}
                {primary ? " ★" : ""}
              </span>
              <span className="text-[10px] tabular-nums text-slate-400">
                {m.width}×{m.height}
              </span>
              <div className="mt-1 flex gap-0.5">
                {primary ? null : (
                  <IconButton label="Make primary (monitor 0)" onClick={() => onPrimary(i)}>
                    <Star className="size-3.5" />
                  </IconButton>
                )}
                <IconButton
                  label={matching === i ? "Asking its window…" : "Match its viewer window's size"}
                  disabled={matching !== null}
                  onClick={() => onMatchWindow(i)}
                >
                  <Maximize2 className="size-3.5" />
                </IconButton>
                {monitors.length > 1 ? (
                  <IconButton label="Remove this monitor" danger onClick={() => onRemove(i)}>
                    <Trash2 className="size-3.5" />
                  </IconButton>
                ) : null}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

/** A small icon button in a monitor's box, its label in a tooltip above it on hover. A press on
 *  it starts no drag. */
function IconButton({
  label,
  danger = false,
  disabled = false,
  onClick,
  children,
}: {
  label: string;
  danger?: boolean;
  disabled?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      disabled={disabled}
      onPointerDown={(e) => e.stopPropagation()}
      onClick={onClick}
      className={`group relative rounded p-1 disabled:opacity-40 ${
        danger
          ? "text-rose-500 hover:bg-rose-100 dark:hover:bg-rose-950/60"
          : "text-slate-500 hover:bg-slate-200 hover:text-slate-800 dark:text-slate-400 dark:hover:bg-slate-700 dark:hover:text-slate-100"
      }`}
    >
      {children}
      <span
        role="tooltip"
        className="pointer-events-none absolute bottom-full left-1/2 z-20 mb-1 -translate-x-1/2 whitespace-nowrap rounded bg-slate-900 px-1.5 py-0.5 text-[10px] font-medium text-white opacity-0 shadow transition-opacity group-hover:opacity-100 dark:bg-slate-100 dark:text-slate-900"
      >
        {label}
      </span>
    </button>
  );
}
