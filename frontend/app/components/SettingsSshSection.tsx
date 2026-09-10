// The SSH Access section's body: the public keys installed on the bastion and every clone,
// and the bastion port those keys are used against. The copied `ssh -J …` command targets
// this page's own address (the public-host override is gone — every rig left it empty).
//
// The port is read-only here because it is hardcoded on the server, so it is shown for
// reference rather than edited.
import { Field, settingsInput } from "~/components/SettingsFields";

export function SettingsSshSection({
  authorizedKeys,
  bastionPort,
  onAuthorizedKeysChange,
}: {
  /** One full `ssh-ed25519 AAAA… comment` line each. */
  authorizedKeys: string[];
  /** The bastion `sshd` port, for reference. Hardcoded on the server. */
  bastionPort: number;
  onAuthorizedKeysChange: (keys: string[]) => void;
}) {
  return (
    <div className="space-y-3">
      <Field label="Authorized public keys (one per line)">
        <textarea
          value={authorizedKeys.join("\n")}
          onChange={(e) =>
            onAuthorizedKeysChange(e.target.value.split("\n").filter((line) => line.trim() !== ""))
          }
          placeholder="ssh-ed25519 AAAA… me@laptop"
          spellCheck={false}
          rows={4}
          className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
        />
      </Field>
      <div>
        <span className="mb-0.5 block text-xs font-medium text-slate-500 dark:text-slate-400">
          Bastion port
        </span>
        <input
          value={bastionPort}
          readOnly
          disabled
          className={`${settingsInput} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
        />
        <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
          The bastion `sshd` port clones are reached through. Fixed at startup.
        </p>
      </div>
    </div>
  );
}
