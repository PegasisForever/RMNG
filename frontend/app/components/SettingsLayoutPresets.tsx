// The Layout presets section's body: named monitor arrangements, each with its own geometry
// editor. The active one is switched from the rail, not here — this is where the
// arrangements are made.
//
// The whole list is one prop and one `onChange`. Adding, renaming, dropping a preset and
// editing its monitors are all a new array handed back, so this component holds no state and
// a story can drive it with `useState`.
import { MonitorsEditor, type Mon } from "~/components/MonitorsEditor";
import { settingsInput } from "~/components/SettingsFields";
import { newLayoutPreset, type LayoutPresetDraft } from "~/lib/settingsDraft";
import type { ScrollMode } from "~/lib/wire/ScrollMode";

export function SettingsLayoutPresets({
  presets,
  onChange,
}: {
  presets: LayoutPresetDraft[];
  onChange: (presets: LayoutPresetDraft[]) => void;
}) {
  const replace = (i: number, next: Partial<LayoutPresetDraft>) =>
    onChange(presets.map((p, j) => (j === i ? { ...p, ...next } : p)));

  return (
    <div className="space-y-3">
      {presets.length === 0 ? (
        <p className="text-xs text-slate-400 dark:text-slate-500">No layout presets.</p>
      ) : null}
      {presets.map((p, i) => (
        <div key={i} className="rounded border border-slate-200 p-3 dark:border-slate-700">
          <div className="mb-2 flex items-center gap-2">
            <input
              className={settingsInput}
              placeholder="preset name (e.g. Dual 1440p)"
              value={p.name}
              onChange={(e) => replace(i, { name: e.target.value })}
            />
            <button
              type="button"
              onClick={() => onChange(presets.filter((_, j) => j !== i))}
              className="rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100 dark:text-slate-400 dark:hover:bg-slate-800"
            >
              Remove
            </button>
          </div>
          <MonitorsEditor
            monitors={p.monitors}
            onChange={(mons: Mon[]) => replace(i, { monitors: mons })}
          />
          {/* Applied in every viewer window as soon as this layout is the active one. */}
          <div className="mt-2 flex items-center gap-2 text-xs text-slate-600 dark:text-slate-300">
            <label className="flex items-center gap-1">
              Scroll
              <select
                className={settingsInput}
                value={p.scrollMode}
                onChange={(e) => replace(i, { scrollMode: e.target.value as ScrollMode })}
              >
                <option value="auto">Auto (mouse wheel or trackpad, per event)</option>
                <option value="mouse">Mouse wheel</option>
                <option value="trackpad">Trackpad</option>
              </select>
            </label>
            <label className="flex items-center gap-1">
              Speed
              <input
                type="number"
                min={10}
                max={1000}
                step={10}
                className={`${settingsInput} w-20`}
                value={p.scrollSpeed}
                onChange={(e) => replace(i, { scrollSpeed: Number(e.target.value) })}
              />
              %
            </label>
          </div>
        </div>
      ))}
      <button
        type="button"
        onClick={() => onChange([...presets, newLayoutPreset()])}
        className="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-800"
      >
        + Add layout preset
      </button>
    </div>
  );
}
