// Step 2 of the first-run wizard: the fleet's server-side defaults. Clone naming, the
// per-clone resource limits, the monitor arrangement every clone boots with, the viewer's
// chroma mode, and the listen ports.
//
// The ports are collapsed by default because a first run almost never changes them, and the
// four numbers underneath are the ones a wrong guess makes unreachable. The open flag is a
// prop rather than local state so the collapsed and expanded forms are both a story.
import { ChevronDown, ChevronRight } from "lucide-react";

import { MonitorsEditor, type Mon } from "~/components/MonitorsEditor";
import { Field, settingsInput } from "~/components/SettingsFields";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ListenConfig } from "~/lib/wire/ListenConfig";

export function SetupServerStep({
  hostnamePrefix,
  cloneCpus,
  cloneMemoryMb,
  monitors,
  chroma,
  listen,
  agentPort,
  portsOpen,
  onHostnamePrefixChange,
  onCloneCpusChange,
  onCloneMemoryMbChange,
  onMonitorsChange,
  onChromaChange,
  onListenChange,
  onAgentPortChange,
  onPortsOpenChange,
}: {
  hostnamePrefix: string;
  cloneCpus: number;
  cloneMemoryMb: number;
  /** The one arrangement the wizard edits. There is no preset picker here: which named
   *  preset this becomes is decided by the config, not by the operator. */
  monitors: Mon[];
  chroma: ChromaMode;
  listen: ListenConfig;
  agentPort: number;
  /** The ports block is expanded. */
  portsOpen: boolean;
  onHostnamePrefixChange: (value: string) => void;
  onCloneCpusChange: (value: number) => void;
  onCloneMemoryMbChange: (value: number) => void;
  onMonitorsChange: (monitors: Mon[]) => void;
  onChromaChange: (chroma: ChromaMode) => void;
  onListenChange: (listen: ListenConfig) => void;
  onAgentPortChange: (port: number) => void;
  onPortsOpenChange: (open: boolean) => void;
}) {
  return (
    <div className="space-y-4">
      <p className="text-sm text-slate-600 dark:text-slate-300">
        Server-side layout and defaults for the fleet.
      </p>

      <Field label="Clone hostname prefix">
        <input
          value={hostnamePrefix}
          onChange={(e) => onHostnamePrefixChange(e.target.value)}
          placeholder="pega-"
          spellCheck={false}
          className={settingsInput}
        />
        <span className="mt-0.5 block text-xs text-slate-400 dark:text-slate-500">
          Prepended to derived clone hostnames — e.g.{" "}
          <code>{hostnamePrefix || "pega-"}</code>dev-123.
        </span>
      </Field>

      <div className="grid grid-cols-2 gap-3">
        <Field label="CPU limit per clone (cores)">
          <input
            type="number"
            min={1}
            value={cloneCpus}
            onChange={(e) => onCloneCpusChange(Number(e.target.value) || 0)}
            className={settingsInput}
          />
        </Field>
        <Field label="Memory limit per clone (MB)">
          <input
            type="number"
            min={1024}
            value={cloneMemoryMb}
            onChange={(e) => onCloneMemoryMbChange(Number(e.target.value) || 0)}
            className={settingsInput}
          />
        </Field>
      </div>

      <div>
        <span className="mb-1 block text-xs font-medium text-slate-500 dark:text-slate-400">Monitors</span>
        <MonitorsEditor monitors={monitors} onChange={onMonitorsChange} />
      </div>

      <Field label="Chroma mode">
        <select
          value={chroma}
          onChange={(e) => onChromaChange(e.target.value as ChromaMode)}
          className={settingsInput}
        >
          <option value="yuv420">4:2:0 (default)</option>
          <option value="yuv444">4:4:4 (AVC444, ≤1440p/monitor)</option>
        </select>
      </Field>

      {/* Ports — collapsed by default. */}
      <div className="border-t border-slate-100 dark:border-slate-800 pt-3">
        <button
          type="button"
          onClick={() => onPortsOpenChange(!portsOpen)}
          className="flex items-center gap-1 text-xs font-medium text-slate-500 dark:text-slate-400 hover:text-slate-700 dark:hover:text-slate-200"
        >
          {portsOpen ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
          {portsOpen ? "Hide" : "Show"} ports
        </button>
        {portsOpen ? (
          <div className="mt-2 grid grid-cols-2 gap-3">
            {(["web", "video", "daemonMcp"] as const).map((k) => (
              <Field key={k} label={`Port: ${k}`}>
                <input
                  type="number"
                  value={listen[k]}
                  onChange={(e) =>
                    onListenChange({ ...listen, [k]: Number(e.target.value) || 0 })
                  }
                  className={settingsInput}
                />
              </Field>
            ))}
            <Field label="Agent-wrapper port">
              <input
                type="number"
                value={agentPort}
                onChange={(e) => onAgentPortChange(Number(e.target.value) || 0)}
                className={settingsInput}
              />
            </Field>
          </div>
        ) : null}
      </div>
    </div>
  );
}
