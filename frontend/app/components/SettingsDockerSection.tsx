// The Docker / Clones section's body: the Docker probe, the clone naming setting,
// and the per-clone resource limits. (The clone subnet used to be editable here during
// first-run setup; it is hardcoded on the server now.)
//
// Each field carries its own effect badge because this section's header has none: the
// settings under it do not all take effect at the same moment.
import {
  EffectBadge,
  FieldHeading,
  settingsInput,
} from "~/components/SettingsFields";

export function SettingsDockerSection({
  hostnamePrefix,
  cloneCpus,
  cloneMemoryMb,
  testMessage,
  onHostnamePrefixChange,
  onCloneCpusChange,
  onCloneMemoryMbChange,
  onTest,
}: {
  hostnamePrefix: string;
  cloneCpus: number;
  cloneMemoryMb: number;
  /** The result of the last Docker probe, in the panel's own words. */
  testMessage: string | null;
  onHostnamePrefixChange: (value: string) => void;
  onCloneCpusChange: (value: number) => void;
  onCloneMemoryMbChange: (value: number) => void;
  onTest: () => void;
}) {
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onTest}
          className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          Test Docker
        </button>
        {testMessage ? (
          <p className="text-xs text-slate-500 dark:text-slate-400">
            {testMessage}
          </p>
        ) : null}
      </div>
      <div>
        <FieldHeading label="Clone hostname prefix" effect="immediate" />
        <input
          value={hostnamePrefix}
          onChange={(e) => onHostnamePrefixChange(e.target.value)}
          placeholder="pega-"
          className={`mt-0.5 ${settingsInput}`}
        />
        <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
          Prepended to derived clone hostnames — e.g.{" "}
          <code>{hostnamePrefix || "pega-"}</code>dev-123 /{" "}
          <code>{hostnamePrefix || "pega-"}</code>my-task. Lowercased +
          sanitized to a DNS label; blank keeps the current value.
        </p>
      </div>
      <div className="grid grid-cols-2 gap-3">
        <div>
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-slate-500 dark:text-slate-400">
              CPU limit per clone (cores)
            </span>
            <EffectBadge effect="immediate" />
          </div>
          <input
            type="number"
            min={1}
            value={cloneCpus}
            onChange={(e) => onCloneCpusChange(Number(e.target.value) || 0)}
            className={`mt-0.5 ${settingsInput}`}
          />
        </div>
        <div>
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-slate-500 dark:text-slate-400">
              Memory limit per clone (MB)
            </span>
            <EffectBadge effect="immediate" />
          </div>
          <input
            type="number"
            min={1024}
            value={cloneMemoryMb}
            onChange={(e) => onCloneMemoryMbChange(Number(e.target.value) || 0)}
            className={`mt-0.5 ${settingsInput}`}
          />
        </div>
      </div>
      <p className="text-xs text-slate-400 dark:text-slate-500">
        Limits apply to newly created clones (existing clones keep the limits
        they were created with).
      </p>
    </div>
  );
}
