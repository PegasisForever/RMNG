// The Presets section's body: one card per preset, holding its Linear identity (the API key
// plus the ticket-id prefixes that auto-select it), its two per-provider account defaults,
// its env vars, and its two prompt appendices.
//
// The whole list is one prop and one `onChange`, so every edit inside a card is a new array
// handed back and nothing here is stateful. The blank option on each account picker means
// "no default" rather than `auto`: a preset with no opinion lets the clone decide, whereas
// `auto` is an explicit choice a sub clone would inherit.
import { X } from "lucide-react";

import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { Field, Secret, settingsInput } from "~/components/SettingsFields";
import type { ClaudeUsage } from "~/lib/types";
import { newPreset, type GroupDraft, type PresetDraft } from "~/lib/settingsDraft";

export function SettingsPresetList({
  presets,
  accounts,
  claudeGroups,
  codexGroups,
  onChange,
}: {
  presets: PresetDraft[];
  /** Both providers' rows, flat and tagged by `provider`. Each picker takes its own side. */
  accounts: ClaudeUsage[];
  /** The Claude pools the form currently holds, so a pool renamed above is offered here. */
  claudeGroups: GroupDraft[];
  /** The Codex pools the form currently holds. */
  codexGroups: GroupDraft[];
  onChange: (presets: PresetDraft[]) => void;
}) {
  const replace = (i: number, next: Partial<PresetDraft>) =>
    onChange(presets.map((p, j) => (j === i ? { ...p, ...next } : p)));

  return (
    <div className="space-y-3">
      {presets.length === 0 ? <p className="text-xs text-slate-400 dark:text-slate-500">No presets.</p> : null}
      {presets.map((p, i) => (
        <div key={i} className="rounded border border-slate-200 dark:border-slate-700 p-3">
          <div className="flex items-center gap-2">
            <input
              value={p.name}
              onChange={(e) => replace(i, { name: e.target.value })}
              placeholder="preset name"
              className={settingsInput}
            />
            <button
              type="button"
              onClick={() => onChange(presets.filter((_, j) => j !== i))}
              className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800"
            >
              Remove
            </button>
          </div>
          <div className="mt-2">
            <input
              value={p.labels}
              onChange={(e) => replace(i, { labels: e.target.value })}
              placeholder="Ticket-id prefixes / team keys, comma-separated, e.g. DEV, WE (auto-selects this preset)"
              spellCheck={false}
              className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
            />
          </div>
          <div className="mt-1.5">
            <Secret
              label="Linear API key"
              set={p.keySet}
              value={p.linearKey}
              onChange={(v) => replace(i, { linearKey: v })}
            />
          </div>
          {/* Default account per provider. Blank is "no default" — a clone of this
              preset then falls through to `auto`, which is NOT the same as pinning
              it to auto here (an explicit choice a sub clone would inherit). */}
          <div className="mt-1.5 flex gap-2">
            <AccountGroupSelect
              groups={claudeGroups}
              accounts={accounts.filter((a) => (a.provider ?? "claude") === "claude")}
              value={p.claudeAccount}
              blankLabel="Claude: no default"
              onChange={(v) => replace(i, { claudeAccount: v })}
              className="w-1/2 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
            />
            <AccountGroupSelect
              groups={codexGroups}
              accounts={accounts.filter((a) => a.provider === "codex")}
              value={p.codexAccount}
              blankLabel="Codex: no default"
              onChange={(v) => replace(i, { codexAccount: v })}
              className="w-1/2 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
            />
          </div>
          <div className="mt-2 space-y-1.5">
            {p.vars.map((v, k) => (
              <div key={k} className="flex items-center gap-2">
                <input
                  value={v.key}
                  onChange={(e) =>
                    replace(i, {
                      vars: p.vars.map((vv, m) => (m === k ? { ...vv, key: e.target.value } : vv)),
                    })
                  }
                  placeholder="KEY"
                  spellCheck={false}
                  className="w-2/5 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
                />
                <span className="text-slate-400 dark:text-slate-500">=</span>
                <input
                  value={v.value}
                  onChange={(e) =>
                    replace(i, {
                      vars: p.vars.map((vv, m) => (m === k ? { ...vv, value: e.target.value } : vv)),
                    })
                  }
                  placeholder="value"
                  spellCheck={false}
                  className="flex-1 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
                />
                <button
                  type="button"
                  onClick={() => replace(i, { vars: p.vars.filter((_, m) => m !== k) })}
                  title="remove variable"
                  className="shrink-0 rounded px-2 py-1 text-xs text-slate-400 dark:text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800"
                >
                  <X className="size-4" />
                </button>
              </div>
            ))}
          </div>
          <button
            type="button"
            onClick={() => replace(i, { vars: [...p.vars, { key: "", value: "" }] })}
            className="mt-2 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
          >
            + Add variable
          </button>
          <div className="mt-2">
            <Field label="Extra global prompt for this preset (appended to the global agent prompt, for every agent)">
              <textarea
                value={p.globalPrompt}
                onChange={(e) => replace(i, { globalPrompt: e.target.value })}
                spellCheck={false}
                rows={4}
                placeholder="(optional)"
                className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
              />
            </Field>
          </div>
          <div className="mt-2">
            <Field label="Extra node-agent prompt for this preset (appended to the node-agent prompt only)">
              <textarea
                value={p.agentPlaybook}
                onChange={(e) => replace(i, { agentPlaybook: e.target.value })}
                spellCheck={false}
                rows={4}
                placeholder="(optional)"
                className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
              />
            </Field>
          </div>
        </div>
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
