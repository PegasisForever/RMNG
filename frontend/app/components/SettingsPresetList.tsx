// The Presets section's body: one card per preset, holding its name, the ticket-id
// prefixes that auto-select it, its Linear API key (a regular visible field — what the
// editor sends is what is stored, blank clears it), its default account pool (one pool
// feeds both providers), its full Dockerfile (built lazily into the preset's image; the rebuild
// button below always pulls fresh base and rebuilds), and its two prompt appendices.
//
// The whole list is one prop and one `onChange`, so every edit inside a card is a new
// array handed back and nothing here is stateful — except each card's rebuild status,
// which is local to the card (the build it starts is followed in Jobs, not here).
import { useState } from "react";

import { Field, settingsInput } from "~/components/SettingsFields";
import { prebuildDockerfile } from "~/lib/api";
import {
  newPreset,
  type GroupDraft,
  type PresetDraft,
} from "~/lib/settingsDraft";

export function SettingsPresetList({
  presets,
  groups,
  forkSources,
  onChange,
}: {
  presets: PresetDraft[];
  /** The pools the form currently holds, so a pool renamed above is offered here. */
  groups: GroupDraft[];
  /** Forkable clone ids, oldest first, for the default-source picker. */
  forkSources: string[];
  onChange: (presets: PresetDraft[]) => void;
}) {
  const replace = (i: number, next: Partial<PresetDraft>) =>
    onChange(presets.map((p, j) => (j === i ? { ...p, ...next } : p)));
  // A preset always names a pool default: a new preset points at the first pool.
  const firstGroup = groups.find((g) => g.name.trim())?.name.trim() ?? "none";

  return (
    <div className="space-y-3">
      {presets.length === 0 ? (
        <p className="text-xs text-slate-400 dark:text-slate-500">
          No presets.
        </p>
      ) : null}
      {presets.map((p, i) => (
        <PresetCard
          key={i}
          preset={p}
          groups={groups}
          forkSources={forkSources}
          onChange={(next) => replace(i, next)}
          onRemove={() => onChange(presets.filter((_, j) => j !== i))}
        />
      ))}
      <button
        type="button"
        onClick={() => onChange([...presets, newPreset(firstGroup)])}
        className="rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
      >
        + Add preset
      </button>
    </div>
  );
}

function PresetCard({
  preset: p,
  groups,
  forkSources,
  onChange,
  onRemove,
}: {
  preset: PresetDraft;
  groups: GroupDraft[];
  forkSources: string[];
  onChange: (next: Partial<PresetDraft>) => void;
  onRemove: () => void;
}) {
  // Pools on offer, in form order. A value naming a deleted pool (or a blank from an
  // older save) still renders so the card never shows a choice it does not hold.
  const poolNames = groups.map((g) => g.name.trim()).filter(Boolean);
  const trimmed = p.group.trim();
  // A blank from an older save reads as the first pool; anything else renders as held
  // (a deleted pool gets its own marked option below).
  const selected = trimmed === "" ? (poolNames[0] ?? "none") : trimmed;
  // The last rebuild started from this card's editor text. The build itself runs as a
  // Jobs op (followed there); this is only the receipt: which tag was warmed, or why
  // the start failed.
  const [rebuild, setRebuild] = useState<string | null>(null);
  const [rebuilding, setRebuilding] = useState(false);

  const rebuildImage = () => {
    if (rebuilding) return;
    setRebuilding(true);
    setRebuild(null);
    prebuildDockerfile(p.dockerfile)
      .then((op) =>
        setRebuild(`build queued → ${op.target} (follow it in Jobs)`),
      )
      .catch((e: Error) => setRebuild(`rebuild failed: ${e.message}`))
      .finally(() => setRebuilding(false));
  };

  return (
    <div className="rounded border border-slate-200 dark:border-slate-700 p-3">
      <div className="flex items-end gap-2">
        <div className="flex-1">
          <Field label="Preset name">
            <input
              value={p.name}
              onChange={(e) => onChange({ name: e.target.value })}
              placeholder="preset name"
              className={settingsInput}
            />
          </Field>
        </div>
        <button
          type="button"
          onClick={onRemove}
          className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800"
        >
          Remove
        </button>
      </div>
      <div className="mt-2">
        <Field label="Ticket-id prefixes / team keys (auto-selects this preset)">
          <input
            value={p.labels}
            onChange={(e) => onChange({ labels: e.target.value })}
            placeholder="comma-separated, e.g. DEV, WE"
            spellCheck={false}
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
        </Field>
      </div>
      <div className="mt-2">
        <Field label="Linear API key (visible; blank clears it)">
          <input
            value={p.linearKey}
            onChange={(e) => onChange({ linearKey: e.target.value })}
            placeholder="(none)"
            spellCheck={false}
            autoComplete="off"
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
        </Field>
      </div>
      {/* Default pool, always set: a named pool, or any group (rmng picks whichever
          account is free in any pool). Specific accounts are picked per clone, not
          per preset. */}
      <div className="mt-2">
        <Field label="Default group (both Claude and Codex draw from it)">
          <select
            value={selected}
            onChange={(e) => onChange({ group: e.target.value })}
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
          >
            <option value="none">Any group (rmng picks a free account)</option>
            {poolNames.map((name) => {
              const members =
                groups.find((g) => g.name.trim() === name)?.accounts.length ??
                0;
              return (
                <option key={name} value={name}>
                  {name} ({members})
                </option>
              );
            })}
            {selected !== "none" && !poolNames.includes(selected) ? (
              <option value={selected}>{selected} (deleted pool)</option>
            ) : null}
          </select>
        </Field>
      </div>
      {/* Default fork source for the clone modal's fork tabs. Blank = oldest forkable
          clone; a stale id falls back the same way, so a deleted clone never strands
          the preset. */}
      <div className="mt-2">
        <Field label="Default fork clone (fork tabs)">
          <select
            value={
              forkSources.includes(p.defaultForkClone.trim())
                ? p.defaultForkClone.trim()
                : ""
            }
            onChange={(e) => onChange({ defaultForkClone: e.target.value })}
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
          >
            <option value="">Oldest forkable clone</option>
            {forkSources.map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
            {p.defaultForkClone.trim() !== "" &&
            !forkSources.includes(p.defaultForkClone.trim()) ? (
              <option value={p.defaultForkClone.trim()}>
                {p.defaultForkClone.trim()} (gone — falls back to oldest)
              </option>
            ) : null}
          </select>
        </Field>
      </div>
      <div className="mt-2">
        <Field label="Dockerfile (full; creates reuse the built image — tick Rebuild in the New clone dialog or use the button below for a fresh base pull)">
          <textarea
            value={p.dockerfile}
            onChange={(e) => onChange({ dockerfile: e.target.value })}
            spellCheck={false}
            rows={8}
            placeholder="FROM pegasis0/rmng-template:latest"
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
        </Field>
        <div className="mt-1.5 flex items-center gap-2">
          <button
            type="button"
            onClick={rebuildImage}
            disabled={rebuilding}
            className="rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800 disabled:opacity-50"
          >
            {rebuilding ? "Starting…" : "Rebuild image"}
          </button>
          {rebuild ? (
            <span className="text-[11px] text-slate-500 dark:text-slate-400">
              {rebuild}
            </span>
          ) : null}
        </div>
      </div>
      <div className="mt-2">
        <Field label="Extra global prompt for this preset (appended to the global agent prompt, for every agent)">
          <textarea
            value={p.globalPrompt}
            onChange={(e) => onChange({ globalPrompt: e.target.value })}
            spellCheck={false}
            rows={4}
            placeholder="(optional)"
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
        </Field>
      </div>
      <div className="mt-2">
        <Field label="Startup script for this preset (runs as the clone user as the last create/fork step, when the request opts in)">
          <textarea
            value={p.startupScript}
            onChange={(e) => onChange({ startupScript: e.target.value })}
            spellCheck={false}
            rows={4}
            placeholder="e.g. ~/bin/setup-env.sh (optional — empty runs nothing)"
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
        </Field>
      </div>
      <div className="mt-2">
        <Field label="Extra node-agent prompt for this preset (appended to the node-agent prompt only)">
          <textarea
            value={p.agentPlaybook}
            onChange={(e) => onChange({ agentPlaybook: e.target.value })}
            spellCheck={false}
            rows={4}
            placeholder="(optional)"
            className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
        </Field>
      </div>
    </div>
  );
}
