// The Presets section's body: one card per preset, holding its name, the ticket-id
// prefixes that auto-select it, its Linear API key (a regular visible field — what the
// editor sends is what is stored, blank clears it), its two per-provider account
// defaults, its full Dockerfile (built lazily into the preset's image, same text never
// rebuilds), and its two prompt appendices.
//
// The whole list is one prop and one `onChange`, so every edit inside a card is a new
// array handed back and nothing here is stateful — except each card's rebuild status,
// which is local to the card (the build it starts is followed in Jobs, not here).
import { useState } from "react";

import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { Field, settingsInput } from "~/components/SettingsFields";
import { prebuildDockerfile } from "~/lib/api";
import type { ClaudeUsage } from "~/lib/types";
import { newPreset, type GroupDraft, type PresetDraft } from "~/lib/settingsDraft";

export function SettingsPresetList({
  presets,
  accounts,
  groups,
  onChange,
}: {
  presets: PresetDraft[];
  /** Both providers' rows, flat and tagged by `provider`. Each picker takes its own side. */
  accounts: ClaudeUsage[];
  /** The pools the form currently holds, so a pool renamed above is offered here. */
  groups: GroupDraft[];
  onChange: (presets: PresetDraft[]) => void;
}) {
  const replace = (i: number, next: Partial<PresetDraft>) =>
    onChange(presets.map((p, j) => (j === i ? { ...p, ...next } : p)));

  return (
    <div className="space-y-3">
      {presets.length === 0 ? <p className="text-xs text-slate-400 dark:text-slate-500">No presets.</p> : null}
      {presets.map((p, i) => (
        <PresetCard
          key={i}
          preset={p}
          accounts={accounts}
          groups={groups}
          onChange={(next) => replace(i, next)}
          onRemove={() => onChange(presets.filter((_, j) => j !== i))}
        />
      ))}
      <button
        type="button"
        onClick={() => onChange([...presets, newPreset()])}
        className="rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
      >
        + Add preset
      </button>
    </div>
  );
}

function PresetCard({
  preset: p,
  accounts,
  groups,
  onChange,
  onRemove,
}: {
  preset: PresetDraft;
  accounts: ClaudeUsage[];
  groups: GroupDraft[];
  onChange: (next: Partial<PresetDraft>) => void;
  onRemove: () => void;
}) {
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
      .then((op) => setRebuild(`build queued → ${op.target} (follow it in Jobs)`))
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
      {/* Default account per provider. Blank is "no default" — a clone of this
          preset then falls through to `auto`, which is NOT the same as pinning
          it to auto here (an explicit choice a sub clone would inherit). */}
      <div className="mt-2 flex gap-2">
        <div className="w-1/2">
          <Field label="Claude default">
            <AccountGroupSelect
              groups={groups}
              accounts={accounts.filter((a) => (a.provider ?? "claude") === "claude")}
              value={p.claudeAccount}
              blankLabel="Claude: no default"
              onChange={(v) => onChange({ claudeAccount: v })}
              className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
            />
          </Field>
        </div>
        <div className="w-1/2">
          <Field label="Codex default">
            <AccountGroupSelect
              groups={groups}
              accounts={accounts.filter((a) => a.provider === "codex")}
              value={p.codexAccount}
              blankLabel="Codex: no default"
              onChange={(v) => onChange({ codexAccount: v })}
              className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
            />
          </Field>
        </div>
      </div>
      <div className="mt-2">
        <Field label="Dockerfile (full; same text never rebuilds — base releases do not invalidate it)">
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
            <span className="text-[11px] text-slate-500 dark:text-slate-400">{rebuild}</span>
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
