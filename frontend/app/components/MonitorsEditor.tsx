export type Mon = { width: number; height: number; x: number; y: number; primary: boolean };

const PRESETS: { label: string; width: number; height: number }[] = [
  { label: "1080p", width: 1920, height: 1080 },
  { label: "1440p", width: 2560, height: 1440 },
];

/** Graphical preview of the monitor arrangement (scaled to fit). Primary is highlighted. */
function LayoutPreview({ monitors }: { monitors: Mon[] }) {
  if (monitors.length === 0) return null;
  const minX = Math.min(...monitors.map((m) => m.x));
  const minY = Math.min(...monitors.map((m) => m.y));
  const maxX = Math.max(...monitors.map((m) => m.x + m.width));
  const maxY = Math.max(...monitors.map((m) => m.y + m.height));
  const W = Math.max(1, maxX - minX);
  const H = Math.max(1, maxY - minY);
  const BOX_W = 380;
  const BOX_H = 150;
  const PAD = 10;
  const scale = Math.min((BOX_W - 2 * PAD) / W, (BOX_H - 2 * PAD) / H);
  const offX = (BOX_W - W * scale) / 2;
  const offY = (BOX_H - H * scale) / 2;
  return (
    <svg
      viewBox={`0 0 ${BOX_W} ${BOX_H}`}
      className="mb-3 h-[150px] w-full rounded border border-slate-200 bg-slate-50 dark:border-slate-700 dark:bg-slate-900"
      role="img"
      aria-label="monitor layout preview"
    >
      {monitors.map((m, i) => {
        const x = offX + (m.x - minX) * scale;
        const y = offY + (m.y - minY) * scale;
        const w = m.width * scale;
        const h = m.height * scale;
        return (
          <g key={i}>
            <rect
              x={x}
              y={y}
              width={Math.max(2, w)}
              height={Math.max(2, h)}
              rx={3}
              strokeWidth={1.5}
              className={
                m.primary
                  ? "fill-emerald-100 stroke-emerald-500 dark:fill-emerald-900 dark:stroke-emerald-500"
                  : "fill-white stroke-slate-400 dark:fill-slate-800 dark:stroke-slate-600"
              }
            />
            <text x={x + w / 2} y={y + h / 2 - 3} textAnchor="middle" className="fill-slate-600 text-[11px] font-semibold dark:fill-slate-100">
              {i}
              {m.primary ? " ★" : ""}
            </text>
            <text x={x + w / 2} y={y + h / 2 + 10} textAnchor="middle" className="fill-slate-400 text-[9px] dark:fill-slate-300">
              {m.width}×{m.height}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

/** The monitor layout editor: live preview, per-monitor size/position/primary rows, and
 *  resolution add-buttons. Fully controlled — `monitors` in, `onChange` out — so both the
 *  Settings dialog and the first-run wizard can drive it. */
export function MonitorsEditor({
  monitors,
  onChange,
}: {
  monitors: Mon[];
  onChange: (next: Mon[]) => void;
}) {
  const setMon = (i: number, k: "width" | "height" | "x" | "y", v: number) =>
    onChange(monitors.map((m, j) => (j === i ? { ...m, [k]: v } : m)));
  const setPrimary = (i: number) => onChange(monitors.map((m, j) => ({ ...m, primary: j === i })));
  // New monitor: append at the right edge of the current layout (primary if it's the first).
  const addMon = (p: { width: number; height: number }) => {
    const x = monitors.reduce((mx, m) => Math.max(mx, m.x + m.width), 0);
    onChange([...monitors, { width: p.width, height: p.height, x, y: 0, primary: monitors.length === 0 }]);
  };
  const rmMon = (i: number) => {
    const next = monitors.filter((_, j) => j !== i);
    if (next.length && !next.some((m) => m.primary)) next[0] = { ...next[0], primary: true };
    onChange(next);
  };

  return (
    <>
      <LayoutPreview monitors={monitors} />
      <div className="space-y-1.5">
        <div className="flex items-center gap-2 px-0.5 text-[10px] font-medium uppercase tracking-wide text-slate-400 dark:text-slate-500">
          <span className="w-5">#</span>
          <span className="w-[4.5rem]">width</span>
          <span className="w-[4.5rem]">height</span>
          <span className="w-[4.5rem]">x</span>
          <span className="w-[4.5rem]">y</span>
          <span>primary</span>
        </div>
        {monitors.map((m, i) => (
          <div key={i} className="flex items-center gap-2">
            <span className="w-5 text-xs text-slate-400 dark:text-slate-500">{i}</span>
            {(["width", "height", "x", "y"] as const).map((k) => (
              <input
                key={k}
                type="number"
                value={m[k]}
                min={k === "width" || k === "height" ? 1 : 0}
                onChange={(e) => setMon(i, k, Number(e.target.value) || 0)}
                className="w-[4.5rem] rounded border border-slate-300 px-1.5 py-1 text-sm focus:border-slate-400 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:focus:border-slate-500"
              />
            ))}
            <input
              type="radio"
              name="primaryMonitor"
              checked={m.primary}
              onChange={() => setPrimary(i)}
              title="set as primary"
              className="ml-1 accent-emerald-600"
            />
            <button
              type="button"
              onClick={() => rmMon(i)}
              disabled={monitors.length <= 1}
              className="ml-auto rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100 disabled:opacity-40 dark:text-slate-400 dark:hover:bg-slate-800"
            >
              Remove
            </button>
          </div>
        ))}
      </div>
      <div className="mt-2 flex flex-wrap gap-1.5">
        {PRESETS.map((p) => (
          <button
            key={p.label}
            type="button"
            onClick={() => addMon(p)}
            className="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-800"
          >
            + {p.label} ({p.width}×{p.height})
          </button>
        ))}
      </div>
    </>
  );
}
