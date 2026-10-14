// The Layout presets section's body: named monitor arrangements, each with its own geometry
// editor. The active one is switched from the rail, not here — this is where the
// arrangements are made.
//
// The whole list is one prop and one `onChange`. Adding, renaming, dropping a preset and
// editing its monitors are all a new array handed back, so this component holds no state and
// a story can drive it with `useState`.
import { useState } from "react";

import { DropdownSelect } from "~/components/DropdownSelect";
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
            Scroll
            <div className="w-56">
              <DropdownSelect
                rows={[
                  { value: "auto", label: "Auto (detect per event)" },
                  { value: "mouse", label: "Mouse wheel" },
                  { value: "trackpad", label: "Trackpad" },
                ]}
                value={p.scrollMode}
                onChange={(v) => replace(i, { scrollMode: v as ScrollMode })}
                label="Scroll"
                className={settingsInput}
              />
            </div>
            {/* A negative speed scrolls the other way: -100 is the same distance, reversed. */}
            <label className="flex items-center gap-1">
              Speed
              <div className="w-24">
                <SpeedInput
                  value={p.scrollSpeed}
                  onChange={(scrollSpeed) => replace(i, { scrollSpeed })}
                />
              </div>
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

/** The scroll speed field. It keeps the typed text, so an unfinished number ("-", "1.") stays
 *  in the field while the operator types; only a complete number goes to `onChange`. */
function SpeedInput({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  const [text, setText] = useState(String(value));
  const [seen, setSeen] = useState(value);
  // A new value from outside (the form loaded, a preset was removed) replaces the text.
  if (value !== seen) {
    setSeen(value);
    setText(String(value));
  }
  return (
    <input
      inputMode="decimal"
      className={settingsInput}
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        const n = Number(e.target.value);
        if (e.target.value.trim() !== "" && Number.isFinite(n)) {
          setSeen(n);
          onChange(n);
        }
      }}
    />
  );
}
