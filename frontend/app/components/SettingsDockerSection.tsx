// The Docker / Clones section's body: the Docker probe, the naming and template settings,
// the one-time clone subnet, and the per-clone resource limits.
//
// Each field carries its own effect badge because this section's header has none: the
// settings under it do not all take effect at the same moment, and the subnet does not take
// effect at all once first-run setup has finished.
import { EffectBadge, FieldHeading, settingsInput } from "~/components/SettingsFields";

export function SettingsDockerSection({
  hostnamePrefix,
  templateReference,
  subnet,
  subnetLocked,
  cloneCpus,
  cloneMemoryMb,
  testMessage,
  onHostnamePrefixChange,
  onTemplateReferenceChange,
  onSubnetChange,
  onCloneCpusChange,
  onCloneMemoryMbChange,
  onTest,
}: {
  hostnamePrefix: string;
  templateReference: string;
  subnet: string;
  /** First-run setup has finished, so the subnet is baked into the rmng bridge and every
   *  clone IP and can no longer be changed. */
  subnetLocked: boolean;
  cloneCpus: number;
  cloneMemoryMb: number;
  /** The result of the last Docker probe, in the panel's own words. */
  testMessage: string | null;
  onHostnamePrefixChange: (value: string) => void;
  onTemplateReferenceChange: (value: string) => void;
  onSubnetChange: (value: string) => void;
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
        {testMessage ? <p className="text-xs text-slate-500 dark:text-slate-400">{testMessage}</p> : null}
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
          Prepended to derived clone hostnames — e.g. <code>{hostnamePrefix || "pega-"}</code>dev-123 /{" "}
          <code>{hostnamePrefix || "pega-"}</code>my-task. Lowercased + sanitized to a DNS label; blank keeps
          the current value.
        </p>
      </div>
      <div>
        <FieldHeading label="Template reference" effect="immediate" />
        <input
          value={templateReference}
          onChange={(e) => onTemplateReferenceChange(e.target.value)}
          placeholder="pegasis0/rmng-template:latest"
          spellCheck={false}
          className={`mt-0.5 ${settingsInput}`}
        />
        <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
          Registry <code>repo:tag</code> the wizard/Images panel pulls the clone template
          from. The pulled image keeps this reference and clones are created from it.
          Read fresh on each pull.
        </p>
      </div>
      {/* Subnet is baked into the rmng bridge + every clone's static IP at first-run
          setup, so it's one-time: editable only during first-run setup. */}
      <div>
        <FieldHeading label="Clone network subnet" effect="one-time" />
        <input
          value={subnet}
          onChange={(e) => onSubnetChange(e.target.value)}
          disabled={subnetLocked}
          placeholder="10.99.0.0/24"
          spellCheck={false}
          className={`mt-0.5 ${settingsInput} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
        />
        <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
          {subnetLocked
            ? "Set during first-run setup — baked into the rmng network + clone IPs, cannot be changed."
            : "IPv4 CIDR (/16–/24) for the rmng bridge — .1 gateway, .2 control-server, .10+ clone pool."}
        </p>
      </div>
      <div className="grid grid-cols-2 gap-3">
        <div>
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-slate-500 dark:text-slate-400">CPU limit per clone (cores)</span>
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
            <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Memory limit per clone (MB)</span>
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
        Limits apply to newly created clones (existing clones keep the limits they were
        created with).
      </p>
    </div>
  );
}
