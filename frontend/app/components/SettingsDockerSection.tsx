// The Docker / Clones section's body: the clone naming setting and the per-clone
// resource limits.
import { Field, settingsInput } from "~/components/SettingsFields";

export function SettingsDockerSection({
  hostnamePrefix,
  cloneCpus,
  cloneMemoryMb,
  onHostnamePrefixChange,
  onCloneCpusChange,
  onCloneMemoryMbChange,
}: {
  hostnamePrefix: string;
  cloneCpus: number;
  cloneMemoryMb: number;
  onHostnamePrefixChange: (value: string) => void;
  onCloneCpusChange: (value: number) => void;
  onCloneMemoryMbChange: (value: number) => void;
}) {
  return (
    <div className="space-y-3">
      <div>
        <Field label="Clone hostname prefix">
          <input
            value={hostnamePrefix}
            onChange={(e) => onHostnamePrefixChange(e.target.value)}
            placeholder="pega-"
            className={settingsInput}
          />
        </Field>
        <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
          Prepended to derived clone hostnames — e.g.{" "}
          <code>{hostnamePrefix || "pega-"}</code>dev-123 /{" "}
          <code>{hostnamePrefix || "pega-"}</code>my-task. Lowercased +
          sanitized to a DNS label; blank keeps the current value.
        </p>
      </div>
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
      <p className="text-xs text-slate-400 dark:text-slate-500">
        Limits apply to newly created clones (existing clones keep the limits
        they were created with).
      </p>
    </div>
  );
}
