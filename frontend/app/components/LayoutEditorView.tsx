// The Edit layout dialog: this browser's monitors (`~/lib/clientLayout.ts`), as the clones get
// them while the operator uses this browser. Every change applies at once: there is no Save.
//
// - The arrangement: each monitor a box, moved only by drag and drop. While it is dragged it
//   snaps to touch another monitor's edge, lined up with its edges or its center when close
//   (`~/lib/monitorLayout.ts`); a dashed outline shows where the pointer is. Click one to edit
//   its size, make it monitor 0 (the primary one), or remove it (its viewer window closes).
// - Add a monitor, at the right; a viewer window opens for it.
// - Match window sizes: each monitor takes the size of its open viewer window, in screen pixels
//   divided by the pixel scale (screen pixels per clone pixel).
// - How the viewer reads the wheel, and the scroll speed.
//
// Pure: the layout comes in and every change goes out, with what it does to the monitors'
// numbers (`LayoutChange`). The container saves and applies it, opens and renumbers the viewer
// windows, and asks them for their sizes.
import { Plus, Star, Trash2 } from "lucide-react";
import { type PointerEvent, useEffect, useRef, useState } from "react";

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
  resize,
  snapMonitor,
} from "~/lib/monitorLayout";
import type { MonitorSpec } from "~/lib/wire/MonitorSpec";
import type { ScrollMode } from "~/lib/wire/ScrollMode";

/** The arrangement's drawing, CSS px. */
const VIEW_W = 440;
const VIEW_H = 190;
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
  /** What the last "Match window sizes" did, for the line under the button; null before it ran. */
  matchNote: string | null;
  /** Asking the viewer windows for their sizes. */
  matching: boolean;
  onMatchWindows: () => void;
  /** The dialog is on screen; false plays the exit. */
  open?: boolean;
  /** The dialog has gone (after its exit). */
  onClose: () => void;
}

export function LayoutEditorView({
  layout,
  onChange,
  matchNote,
  matching,
  onMatchWindows,
  open = true,
  onClose,
}: LayoutEditorViewProps) {
  const [selected, setSelected] = useState(0);
  const mons = layout.monitors;
  const sel = Math.min(selected, mons.length - 1);
  const setMonitors = (monitors: MonitorSpec[]) => onChange({ ...layout, monitors });

  return (
    <ModalShell size="md" open={open} onExited={onClose}>
      {(close) => (
        <>
          <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">Edit layout</h3>
          <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
            This browser&apos;s monitors. The clones get them while you use this browser, and another device&apos;s
            while you use that one.
          </p>

          <Arrangement
            monitors={mons}
            selected={sel}
            onSelect={setSelected}
            onDrop={(i, x, y) => setMonitors(dropMonitor(mons, i, x, y))}
          />

          <SelectedMonitor
            key={`${sel}-${mons[sel].width}-${mons[sel].height}`}
            monitors={mons}
            i={sel}
            onResize={(width, height) =>
              setMonitors(resize(mons, mons.map((m, k) => (k === sel ? { width, height } : m))))
            }
            onPrimary={() => {
              onChange({ ...layout, monitors: makePrimary(mons, sel) }, { t: "renumber", map: primaryMap(mons.length, sel) });
              setSelected(0);
            }}
            onRemove={() => {
              onChange({ ...layout, monitors: removeMonitor(mons, sel) }, { t: "renumber", map: removeMap(mons.length, sel) });
              setSelected(Math.max(0, sel - 1));
            }}
          />

          <div className="mt-3 flex flex-wrap items-center gap-2">
            <button
              type="button"
              disabled={mons.length >= MAX_MONITORS}
              onClick={() => {
                onChange({ ...layout, monitors: addMonitor(mons) }, { t: "add", monitor: mons.length });
                setSelected(mons.length);
              }}
              className="inline-flex items-center gap-1 rounded-md border border-slate-200 px-2.5 py-1 text-xs font-medium text-slate-700 hover:bg-slate-100 disabled:opacity-40 dark:border-slate-700 dark:text-slate-200 dark:hover:bg-slate-800"
            >
              <Plus aria-hidden className="size-3.5" />
              Add monitor
            </button>
            <button
              type="button"
              disabled={matching}
              onClick={onMatchWindows}
              className="rounded-md border border-slate-200 px-2.5 py-1 text-xs font-medium text-slate-700 hover:bg-slate-100 disabled:opacity-40 dark:border-slate-700 dark:text-slate-200 dark:hover:bg-slate-800"
            >
              {matching ? "Asking the windows…" : "Match window sizes"}
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
          <p className="mt-1 text-[11px] text-slate-400 dark:text-slate-500">
            {matchNote ??
              `Match window sizes gives each monitor the size of its open viewer window, at ${layout.pixelScale} screen pixel(s) per clone pixel.`}
          </p>

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

/** The monitors drawn to scale. A drag moves one, snapped where it would land; the drop puts
 *  it there (`onDrop`, in monitor pixels). A press with no move selects it. */
function Arrangement({
  monitors,
  selected,
  onSelect,
  onDrop,
}: {
  monitors: MonitorSpec[];
  selected: number;
  onSelect: (i: number) => void;
  onDrop: (i: number, x: number, y: number) => void;
}) {
  // The drawing's scale and offset stay as they were while a drag is on, so the box under the
  // pointer does not run away from it
  const frame = (ms: MonitorSpec[]) => {
    const x0 = Math.min(...ms.map((m) => m.x));
    const y0 = Math.min(...ms.map((m) => m.y));
    const w = Math.max(...ms.map((m) => m.x + m.width)) - x0;
    const h = Math.max(...ms.map((m) => m.y + m.height)) - y0;
    // Room around it to drag into
    const scale = Math.min((VIEW_W - 2 * VIEW_PAD) / (w * 1.5), (VIEW_H - 2 * VIEW_PAD) / (h * 1.5));
    return { scale, ox: (VIEW_W - w * scale) / 2 - x0 * scale, oy: (VIEW_H - h * scale) / 2 - y0 * scale };
  };
  const [drag, setDrag] = useState<{ i: number; sx: number; sy: number; dx: number; dy: number } | null>(null);
  const kept = useRef(frame(monitors));
  if (!drag) kept.current = frame(monitors);
  const f = kept.current;
  // A drag ends wherever the pointer goes up
  useEffect(() => {
    if (!drag) return;
    const up = () => setDrag(null);
    window.addEventListener("pointercancel", up);
    return () => window.removeEventListener("pointercancel", up);
  }, [drag]);

  const down = (i: number) => (e: PointerEvent<SVGRectElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    onSelect(i);
    setDrag({ i, sx: e.clientX, sy: e.clientY, dx: 0, dy: 0 });
  };
  const move = (e: PointerEvent<SVGRectElement>) => {
    if (drag) setDrag({ ...drag, dx: e.clientX - drag.sx, dy: e.clientY - drag.sy });
  };
  const up = () => {
    if (!drag) return;
    const m = monitors[drag.i];
    if (Math.hypot(drag.dx, drag.dy) > 3) onDrop(drag.i, m.x + drag.dx / f.scale, m.y + drag.dy / f.scale);
    setDrag(null);
  };

  return (
    <svg
      viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
      className="mt-3 w-full touch-none select-none rounded-md border border-slate-200 bg-slate-50 dark:border-slate-700 dark:bg-slate-900"
      role="group"
      aria-label="Monitor arrangement"
    >
      {monitors.map((m, i) => {
        const moving = drag?.i === i;
        const w = m.width * f.scale;
        const h = m.height * f.scale;
        // Dragged: drawn where it would land, the pointer's place dashed
        const raw = moving ? { x: m.x + drag.dx / f.scale, y: m.y + drag.dy / f.scale } : m;
        const at = (moving && snapMonitor(monitors, i, raw.x, raw.y)) || raw;
        const x = f.ox + at.x * f.scale;
        const y = f.oy + at.y * f.scale;
        return (
          <g key={i} className={moving ? "opacity-90" : undefined}>
            {moving ? (
              <rect
                x={f.ox + raw.x * f.scale}
                y={f.oy + raw.y * f.scale}
                width={w}
                height={h}
                rx={3}
                strokeDasharray="4 3"
                className="pointer-events-none fill-none stroke-slate-400 dark:stroke-slate-500"
              />
            ) : null}
            <rect
              x={x}
              y={y}
              width={w}
              height={h}
              rx={3}
              strokeWidth={i === selected ? 2 : 1.5}
              aria-label={`Monitor ${i}`}
              className={`cursor-grab ${
                i === selected
                  ? "fill-emerald-50 stroke-emerald-500 dark:fill-emerald-950 dark:stroke-emerald-500"
                  : "fill-white stroke-slate-400 dark:fill-slate-800 dark:stroke-slate-600"
              }`}
              onPointerDown={down(i)}
              onPointerMove={move}
              onPointerUp={up}
            />
            <text
              x={x + w / 2}
              y={y + h / 2 - 3}
              textAnchor="middle"
              className="pointer-events-none fill-slate-700 text-[12px] font-semibold dark:fill-slate-100"
            >
              {i === 0 ? `${i} ★` : i}
            </text>
            <text
              x={x + w / 2}
              y={y + h / 2 + 11}
              textAnchor="middle"
              className="pointer-events-none fill-slate-400 text-[10px] dark:fill-slate-400"
            >
              {m.width}×{m.height}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

/** The selected monitor's size (taken on Enter or when the field is left), and its actions. */
function SelectedMonitor({
  monitors,
  i,
  onResize,
  onPrimary,
  onRemove,
}: {
  monitors: MonitorSpec[];
  i: number;
  onResize: (width: number, height: number) => void;
  onPrimary: () => void;
  onRemove: () => void;
}) {
  const m = monitors[i];
  const [w, setW] = useState(String(m.width));
  const [h, setH] = useState(String(m.height));
  const commit = () => {
    const width = Number(w) || m.width;
    const height = Number(h) || m.height;
    if (width !== m.width || height !== m.height) onResize(width, height);
    else {
      setW(String(m.width));
      setH(String(m.height));
    }
  };
  const size = (value: string, set: (v: string) => void, label: string) => (
    <input
      aria-label={label}
      type="number"
      value={value}
      onChange={(e) => set(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => e.key === "Enter" && commit()}
      className={settingsInput.replace("w-full", "w-24")}
    />
  );
  return (
    <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-slate-600 dark:text-slate-300">
      <span className="font-medium">Monitor {i}{i === 0 ? " (primary)" : ""}</span>
      {size(w, setW, "Width")}
      <span aria-hidden>×</span>
      {size(h, setH, "Height")}
      <div className="ml-auto flex gap-1">
        <button
          type="button"
          disabled={i === 0}
          onClick={onPrimary}
          className="inline-flex items-center gap-1 rounded-md px-2 py-1 font-medium hover:bg-slate-100 disabled:opacity-40 dark:hover:bg-slate-800"
        >
          <Star aria-hidden className="size-3.5" />
          Make primary
        </button>
        <button
          type="button"
          disabled={monitors.length < 2}
          onClick={onRemove}
          className="inline-flex items-center gap-1 rounded-md px-2 py-1 font-medium text-rose-600 hover:bg-rose-50 disabled:opacity-40 dark:text-rose-400 dark:hover:bg-rose-950/40"
        >
          <Trash2 aria-hidden className="size-3.5" />
          Remove
        </button>
      </div>
    </div>
  );
}
