// The Edit layout dialog: this browser's monitors (`~/lib/clientLayout.ts`), as the clones get
// them while the operator uses this browser. Every change applies at once: there is no Save.
//
// - The arrangement: each monitor a box, moved only by drag and drop. While it is dragged it
//   snaps to touch another monitor's edge, lined up with its edges or its center when close
//   (`~/lib/monitorLayout.ts`); a dashed outline shows where the pointer is. In each box: its
//   pixel scale (screen pixels per clone pixel in its window; a new one changes its size
//   at once, so the window's screen pixels stay as they were), and its buttons: make it
//   monitor 0 (the primary one), open its window, give it the size of its window (in screen
//   pixels divided by its pixel scale), and remove it (its window closes). Sizes come only from
//   the windows and the scales: there is no field to type one in.
// - Add a monitor, at the right; a window opens for it.
// - How the screen reads the wheel, and the scroll speed.
//
// A browser that cannot open windows (a phone: `canOpenWindows`) has one monitor, its own
// screen: there is nothing to arrange, add, open or remove. Its form has only the pixel scale, as
// a dropdown, and the scroll settings.
//
// While the dialog is open, each monitor window shows its monitor's number large in its middle
// (the container tells them), so the boxes here can be matched to the windows.
//
// Its content is flat on the dialog's panel (`~/desk/GLASS.md`). Its lists are the app's own
// (`DropdownSelect`), not the browser's, and its buttons' names show in tooltips (`data-tip`).
//
// Pure: the layout comes in and every change goes out, with what it does to the monitors'
// numbers (`LayoutChange`). The container saves and applies it, opens and renumbers the monitor
// windows, and asks them for their sizes.
import { ExternalLink, Maximize2, Plus, Star, Trash2 } from "lucide-react";
import { type PointerEvent, type ReactNode, useLayoutEffect, useRef, useState } from "react";

import { DropdownSelect } from "~/components/DropdownSelect";
import { ModalShell } from "~/components/ModalShell";
import {
  BUTTON,
  DANGER_TEXT,
  DONE_BUTTON,
  FIELD,
  ICON_BUTTON,
  TEXT,
  TEXT_2,
  TEXT_3,
} from "~/desk/control/ui";
import { type BrowserLayout, PIXEL_SCALES } from "~/lib/clientLayout";
import {
  MAX_MONITORS,
  addMonitor,
  dropMonitor,
  makePrimary,
  primaryMap,
  removeMap,
  removeMonitor,
  renumber,
  rescale,
  resize,
  snapMonitor,
} from "~/lib/monitorLayout";
import type { MonitorSpec } from "~/lib/wire/MonitorSpec";
import type { ScrollMode } from "~/lib/wire/ScrollMode";

/** The arrangement's height, and the room around it, CSS px. A box narrower or lower than
 *  ROOMY_W × ROOMY_H puts its scale and its buttons on two lines, its number and size on one. */
const VIEW_H = 260;
const VIEW_PAD = 12;
const ROOMY_W = 130;
const ROOMY_H = 84;

const SCROLL_MODES: { value: ScrollMode; label: string }[] = [
  { value: "auto", label: "Auto (mouse or trackpad, per event)" },
  { value: "trackpad", label: "Trackpad" },
  { value: "mouse", label: "Mouse wheel" },
];

/** A pixel scale's row in a list. */
const SCALE_ROWS = PIXEL_SCALES.map((s) => ({ value: String(s), label: `${s}×` }));

/** The dialog's classes: flat (`~/desk/GLASS.md`). */
const LOOK = {
  title: `text-[15px] font-semibold tracking-tight ${TEXT}`,
  text: TEXT_2,
  faint: TEXT_3,
  note: "text-amber-700 dark:text-amber-300",
  area: "rounded-xl border border-black/10 bg-white/40 dark:border-white/10 dark:bg-black/25",
  ghost: "rounded-lg border border-dashed border-black/40 dark:border-white/40",
  box: "rounded-lg border border-black/15 bg-white dark:border-white/20 dark:bg-neutral-800",
  primaryBox: "rounded-lg border border-emerald-500 bg-emerald-50 dark:border-emerald-400/80 dark:bg-emerald-950",
  here: "text-emerald-700 dark:text-emerald-300",
  number: "text-neutral-900 dark:text-white",
  size: "text-black/50 dark:text-white/50",
  scale:
    "rounded-md border border-black/15 bg-white px-1 py-0 text-[11px] tabular-nums text-black/80 dark:border-white/15 dark:bg-neutral-900 dark:text-white/80",
  icon: `${ICON_BUTTON} p-0.5`,
  danger: `rounded-md p-0.5 hover:bg-rose-500/15 dark:hover:bg-rose-500/20 ${DANGER_TEXT}`,
  add: `inline-flex items-center gap-1 px-3 py-1.5 text-xs font-medium disabled:opacity-40 ${BUTTON}`,
  label: TEXT_2,
  input: `w-full ${FIELD} px-2.5 py-1.5 text-sm`,
  done: `${DONE_BUTTON} px-5 py-1.5 text-sm font-semibold`,
};

/** What a change does to the monitors' numbers: a monitor added (its number), or monitors
 *  renumbered (`map[old]` is the new number; null: removed). */
export type LayoutChange = { t: "add"; monitor: number } | { t: "renumber"; map: (number | null)[] };

export interface LayoutEditorViewProps {
  /** This browser's layout. */
  layout: BrowserLayout;
  /** A change, applied at once. */
  onChange: (layout: BrowserLayout, change?: LayoutChange) => void;
  /** Give monitor `i` the size of its window. */
  onMatchWindow: (i: number) => void;
  /** Open monitor `i`'s window; absent where the browser cannot open windows (then there is
   *  one monitor, and none can be added). */
  onOpenWindow?: (i: number) => void;
  /** The monitor of the window the dialog is open in, marked "here"; absent: none of them. */
  here?: number;
  /** The monitor whose window is being asked for its size; null when none is. */
  matching: number | null;
  /** A short note on the last match that did nothing (no window open); null for none. */
  note: string | null;
  /** The dialog has gone (after its exit). */
  onClose: () => void;
}

export function LayoutEditorView({
  layout,
  onChange,
  onMatchWindow,
  onOpenWindow,
  here,
  matching,
  note,
  onClose,
}: LayoutEditorViewProps) {
  const windows = onOpenWindow !== undefined;
  const mons = layout.monitors;
  const scales = layout.scales;
  /** Monitor `i` at pixel scale `to`: its window's screen pixels stay as they were. */
  const scale = (i: number, to: number) =>
    onChange({
      ...layout,
      monitors: resize(mons, mons.map((m, k) => (k === i ? rescale(m, scales[i], to) : m))),
      scales: scales.map((s, k) => (k === i ? to : s)),
    });

  return (
    <ModalShell size="md" onExited={onClose}>
      {(close) => (
        <>
          <h3 className={LOOK.title}>Edit layout</h3>
          <p className={`mt-1 text-xs ${LOOK.text}`}>
            {windows
              ? "This browser's monitors. The clones get them while you use this browser, and another device's while you use that one. Each monitor's window shows its number while this is open."
              : "This device's screen, as the clones get it while you use this device, and another device's while you use that one."}
          </p>

          {windows ? (
            <Arrangement
              monitors={mons}
              scales={scales}
              matching={matching}
              onDrop={(i, x, y) => onChange({ ...layout, monitors: dropMonitor(mons, i, x, y) })}
              onScale={scale}
              onPrimary={(i) => {
                const map = primaryMap(mons.length, i);
                onChange({ ...layout, monitors: makePrimary(mons, i), scales: renumber(scales, map) }, { t: "renumber", map });
              }}
              onMatchWindow={onMatchWindow}
              onOpenWindow={onOpenWindow}
              here={here}
              onRemove={(i) => {
                const map = removeMap(mons.length, i);
                onChange({ ...layout, monitors: removeMonitor(mons, i), scales: renumber(scales, map) }, { t: "renumber", map });
              }}
            />
          ) : (
            // A phone: its one screen, so only its pixel scale
            <label className="mt-3 block" data-tip="Screen pixels per clone pixel">
              <span className={`mb-0.5 block text-xs font-medium ${LOOK.label}`}>Pixel scale</span>
              <DropdownSelect
                rows={SCALE_ROWS}
                value={String(scales[0])}
                onChange={(v) => scale(0, Number(v))}
                label="Pixel scale: screen pixels per clone pixel"
                className={LOOK.input}
              />
            </label>
          )}
          {note ? <p className={`mt-1 text-[11px] ${LOOK.note}`}>{note}</p> : null}

          {windows ? (
            <div className="mt-3 flex items-center gap-2">
              <button
                type="button"
                disabled={mons.length >= MAX_MONITORS}
                onClick={() =>
                  onChange({ ...layout, monitors: addMonitor(mons), scales: [...scales, 1] }, { t: "add", monitor: mons.length })
                }
                className={LOOK.add}
              >
                <Plus aria-hidden className="size-3.5" />
                Add monitor
              </button>
            </div>
          ) : null}

          <div className="mt-3 grid grid-cols-[1fr_7rem] gap-2">
            <label className="block">
              <span className={`mb-0.5 block text-xs font-medium ${LOOK.label}`}>Scroll</span>
              <DropdownSelect
                rows={SCROLL_MODES}
                value={layout.scrollMode}
                onChange={(v) => onChange({ ...layout, scrollMode: v as ScrollMode })}
                label="Scroll"
                className={LOOK.input}
              />
            </label>
            <label className="block">
              <span className={`mb-0.5 block text-xs font-medium ${LOOK.label}`}>Speed %</span>
              <input
                type="number"
                step={5}
                value={layout.scrollSpeed}
                onChange={(e) => onChange({ ...layout, scrollSpeed: Number(e.target.value) || 0 })}
                className={LOOK.input}
              />
            </label>
          </div>

          <div className="mt-4 flex items-center justify-end gap-2">
            <span className={`mr-auto text-[11px] ${LOOK.faint}`}>Changes apply at once.</span>
            <button
              type="button"
              onClick={() => close()}
              className={LOOK.done}
            >
              Done
            </button>
          </div>
        </>
      )}
    </ModalShell>
  );
}

/** The monitors drawn to scale, each with its buttons. A drag moves one, snapped where it
 *  would land; the drop puts it there (`onDrop`, in monitor pixels). */
function Arrangement({
  monitors,
  scales,
  matching,
  onDrop,
  onScale,
  onPrimary,
  onMatchWindow,
  onOpenWindow,
  here,
  onRemove,
}: {
  monitors: MonitorSpec[];
  /** Each monitor's pixel scale. */
  scales: number[];
  matching: number | null;
  onDrop: (i: number, x: number, y: number) => void;
  onScale: (i: number, scale: number) => void;
  onPrimary: (i: number) => void;
  onMatchWindow: (i: number) => void;
  onOpenWindow?: (i: number) => void;
  here?: number;
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
    const scale = Math.min((areaW - 2 * VIEW_PAD) / (w * 1.2), (VIEW_H - 2 * VIEW_PAD) / (h * 1.2));
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
      className={`relative mt-3 w-full touch-none select-none ${LOOK.area}`}
      style={{ height: VIEW_H }}
    >
      {monitors.map((m, i) => {
        const moving = drag?.i === i;
        const box = { width: m.width * f.scale, height: m.height * f.scale };
        // Dragged: drawn where it would land, the pointer's place dashed
        const raw = moving ? { x: m.x + drag.dx / f.scale, y: m.y + drag.dy / f.scale } : m;
        const at = (moving && snapMonitor(monitors, i, raw.x, raw.y)) || raw;
        const primary = i === 0;
        const roomy = box.width >= ROOMY_W && box.height >= ROOMY_H;
        return (
          <div key={i}>
            {moving ? (
              <div
                aria-hidden
                className={`pointer-events-none absolute ${LOOK.ghost}`}
                style={{ left: f.ox + raw.x * f.scale, top: f.oy + raw.y * f.scale, ...box }}
              />
            ) : null}
            <div
              aria-label={`Monitor ${i}${primary ? " (primary)" : ""}`}
              className={`${primary ? LOOK.primaryBox : LOOK.box} absolute flex cursor-grab flex-col items-center justify-center ${
                moving ? "z-10 cursor-grabbing opacity-90" : ""
              }`}
              style={{ left: f.ox + at.x * f.scale, top: f.oy + at.y * f.scale, ...box }}
              onPointerDown={down(i)}
              onPointerMove={move}
              onPointerUp={up}
              onPointerCancel={() => setDrag(null)}
            >
              {here === i ? (
                <span className={`absolute left-1.5 top-1 text-[9px] font-medium leading-none ${LOOK.here}`}>
                  here
                </span>
              ) : null}
              <span className={`flex items-baseline gap-1 ${roomy ? "flex-col items-center gap-0" : ""}`}>
                <span className={`text-xs font-semibold ${LOOK.number}`}>
                  {i}
                  {primary ? " ★" : ""}
                </span>
                <span className={`text-[10px] tabular-nums ${LOOK.size}`}>
                  {m.width}×{m.height}
                </span>
              </span>
              <div
                className={`flex items-center justify-center gap-0.5 ${roomy ? "mt-1" : "mt-0.5 max-w-full flex-wrap"}`}
              >
                {/* A press on it, or in its list (on the body, but a child here for React's
                    events), starts no drag */}
                <div
                  data-tip="Pixel scale: screen pixels per clone pixel in its window"
                  onPointerDown={(e) => e.stopPropagation()}
                  className={roomy ? "" : "flex basis-full justify-center"}
                >
                  <DropdownSelect
                    rows={SCALE_ROWS}
                    value={String(scales[i])}
                    onChange={(v) => onScale(i, Number(v))}
                    label={`Pixel scale of monitor ${i}: screen pixels per clone pixel`}
                    className={LOOK.scale}
                  />
                </div>
                {primary ? null : (
                  <IconButton label="Make primary (monitor 0)" onClick={() => onPrimary(i)}>
                    <Star className="size-3.5" />
                  </IconButton>
                )}
                {onOpenWindow ? (
                  <IconButton label="Open its window" onClick={() => onOpenWindow(i)}>
                    <ExternalLink className="size-3.5" />
                  </IconButton>
                ) : null}
                <IconButton
                  label={matching === i ? "Asking its window…" : "Match its window's size"}
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

/** A small icon button in a monitor's box, its label in a tooltip (`data-tip`). A press on it
 *  starts no drag. */
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
      data-tip={label}
      className={`disabled:opacity-40 ${danger ? LOOK.danger : LOOK.icon}`}
    >
      {children}
    </button>
  );
}
