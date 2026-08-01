// The Advanced section's body: the listen ports and the directories, behind an expander
// because almost nobody touches them and two of the four are read-only anyway.
//
// The expander's open flag is a prop rather than local state, so both halves of this section
// are reachable as their own story.
import { ChevronDown, ChevronRight } from "lucide-react";

import { FieldHeading, settingsInput } from "~/components/SettingsFields";
import type { ListenConfig } from "~/lib/wire/ListenConfig";

export function SettingsAdvancedSection({
  listen,
  agentPort,
  dataDir,
  staticDir,
  cloneSocket,
  open,
  onOpenChange,
  onListenChange,
  onAgentPortChange,
  onStaticDirChange,
}: {
  listen: ListenConfig;
  agentPort: number;
  /** The control-server's WORKDIR inside its container. Read-only: fixed at /data, the
   *  mounted volume. */
  dataDir: string;
  staticDir: string;
  /** The shared unix socket clone-daemons connect to. Read-only: fixed by the container's
   *  mounted sock volume. */
  cloneSocket: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onListenChange: (listen: ListenConfig) => void;
  onAgentPortChange: (port: number) => void;
  onStaticDirChange: (dir: string) => void;
}) {
  return (
    <>
      <button
        type="button"
        onClick={() => onOpenChange(!open)}
        className="flex items-center gap-1 text-xs font-medium text-slate-500 dark:text-slate-400 hover:text-slate-700 dark:hover:text-slate-200"
      >
        {open ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
        {open ? "Hide" : "Show"} ports + directories (restart the control-server to apply)
      </button>
      {open ? (
        <div className="mt-2 grid grid-cols-2 gap-3">
          {/* web/video are wired once at startup → restart-required; daemonMcp applies
              live but must match what clones bake in. */}
          {(["web", "video"] as const).map((k) => (
            <div key={k}>
              <FieldHeading label={`Port: ${k}`} effect="restart" />
              <input
                type="number"
                value={listen[k]}
                onChange={(e) => onListenChange({ ...listen, [k]: Number(e.target.value) || 0 })}
                className={`mt-0.5 ${settingsInput}`}
              />
            </div>
          ))}
          <div>
            <FieldHeading label="Port: daemonMcp" effect="immediate" />
            <input
              type="number"
              value={listen.daemonMcp}
              onChange={(e) => onListenChange({ ...listen, daemonMcp: Number(e.target.value) || 0 })}
              className={`mt-0.5 ${settingsInput}`}
            />
            <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">must match what clones bake in: 9004</p>
          </div>
          <div>
            <FieldHeading label="Agent-wrapper port" effect="immediate" />
            <input
              type="number"
              value={agentPort}
              onChange={(e) => onAgentPortChange(Number(e.target.value) || 0)}
              className={`mt-0.5 ${settingsInput}`}
            />
            <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">must match what clones bake in: 4096</p>
          </div>
          {/* Data dir is the control-server's WORKDIR inside its container: fixed at
              /data (the mounted volume). Shown read-only for reference. */}
          <div>
            <FieldHeading label="Data dir" effect="one-time" />
            <input
              value={dataDir}
              readOnly
              disabled
              className={`mt-0.5 ${settingsInput} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
            />
            <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
              fixed at <code>/data</code> in the container (the mounted volume)
            </p>
          </div>
          {/* The shared unix socket clone-daemons connect to. Fixed by the container's
              mounted sock volume; shown read-only. */}
          <div>
            <FieldHeading label="Clone socket" effect="one-time" />
            <input
              value={cloneSocket}
              readOnly
              disabled
              placeholder="/srv/rmng-sock/clones.sock"
              spellCheck={false}
              className={`mt-0.5 ${settingsInput} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
            />
            <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
              fixed by the container's shared sock volume
            </p>
          </div>
          <div className="col-span-2">
            <FieldHeading label="Static (frontend) dir" effect="restart" />
            <input
              value={staticDir}
              onChange={(e) => onStaticDirChange(e.target.value)}
              spellCheck={false}
              className={`mt-0.5 ${settingsInput}`}
            />
            <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">empty = built-in (embedded) frontend</p>
          </div>
        </div>
      ) : null}
    </>
  );
}
