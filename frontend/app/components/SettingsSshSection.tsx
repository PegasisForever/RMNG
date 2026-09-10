// The SSH Access section's body: the public keys installed on the bastion and every clone.
// The copied `ssh -J …` command targets this page's own address (the public-host override
// is gone — every rig left it empty) through the bastion's hardcoded port (2222).
import { Field } from "~/components/SettingsFields";

export function SettingsSshSection({
  authorizedKeys,
  onAuthorizedKeysChange,
}: {
  /** One full `ssh-ed25519 AAAA… comment` line each. */
  authorizedKeys: string[];
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
    </div>
  );
}
