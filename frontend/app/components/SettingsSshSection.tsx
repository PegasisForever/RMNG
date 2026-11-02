// The SSH Access section's body: the public keys installed on the bastion and every clone.
// The copied `ssh -J …` command targets this page's own address (the public-host override
// is gone — every rig left it empty) through the bastion's hardcoded port (2222).
import { Field, settingsCode } from "~/components/SettingsFields";

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
          className={settingsCode}
        />
      </Field>
    </div>
  );
}
