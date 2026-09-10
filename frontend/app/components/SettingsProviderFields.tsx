// The per-provider settings: which account sits at the top of the usage list, plus the
// Codex-only auto-reset switch. One component, rendered once per provider, because the two
// sections are otherwise the same field. (Usage poll intervals used to be editable here;
// they are hardcoded on the server now.)
import { Field, settingsInput } from "~/components/SettingsFields";

export function SettingsProviderFields({
  pinnedEmail,
  onPinnedEmailChange,
  autoReset,
}: {
  pinnedEmail: string;
  onPinnedEmailChange: (value: string) => void;
  /** The Codex auto-reset switch. Omitted for Claude, which has none. */
  autoReset?: {
    value: boolean;
    onChange: (value: boolean) => void;
  };
}) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <Field label="Pinned account email">
        <input
          value={pinnedEmail}
          onChange={(e) => onPinnedEmailChange(e.target.value)}
          className={settingsInput}
        />
      </Field>
      {autoReset ? (
        <label className="col-span-2 flex items-center gap-2 text-sm text-slate-600 dark:text-slate-300">
          <input
            type="checkbox"
            checked={autoReset.value}
            onChange={(e) => autoReset.onChange(e.target.checked)}
          />
          Auto-use Codex reset credits (when every account is &gt;95% weekly and none
          reset within 24h, spend one banked reset to bring an account back)
        </label>
      ) : null}
    </div>
  );
}
