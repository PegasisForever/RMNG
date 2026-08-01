// The per-provider settings pair: how often usage is polled, and which account sits at the
// top of the usage list. One component, rendered once per provider, because the two sections
// are the same two fields.
//
// Codex carries two extra switches and Claude carries none, so they arrive as one optional
// group rather than as a provider name this component would have to branch on.
import { Field, settingsInput } from "~/components/SettingsFields";

export function SettingsProviderFields({
  pollSecs,
  pinnedEmail,
  onPollSecsChange,
  onPinnedEmailChange,
  codexToggles,
}: {
  pollSecs: number;
  pinnedEmail: string;
  onPollSecsChange: (value: number) => void;
  onPinnedEmailChange: (value: string) => void;
  /** The two Codex-only switches. Omitted for Claude, which has neither. */
  codexToggles?: {
    usagePolling: boolean;
    autoReset: boolean;
    onUsagePollingChange: (value: boolean) => void;
    onAutoResetChange: (value: boolean) => void;
  };
}) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <Field label="Usage poll interval (s)">
        <input
          type="number"
          value={pollSecs}
          onChange={(e) => onPollSecsChange(Number(e.target.value) || 0)}
          className={settingsInput}
        />
      </Field>
      <Field label="Pinned account email">
        <input
          value={pinnedEmail}
          onChange={(e) => onPinnedEmailChange(e.target.value)}
          className={settingsInput}
        />
      </Field>
      {codexToggles ? (
        <>
          <label className="col-span-2 flex items-center gap-2 text-sm text-slate-600 dark:text-slate-300">
            <input
              type="checkbox"
              checked={codexToggles.usagePolling}
              onChange={(e) => codexToggles.onUsagePollingChange(e.target.checked)}
            />
            Poll ChatGPT usage (uncheck if the usage endpoint drifts; refresh + push still run)
          </label>
          <label className="col-span-2 flex items-center gap-2 text-sm text-slate-600 dark:text-slate-300">
            <input
              type="checkbox"
              checked={codexToggles.autoReset}
              onChange={(e) => codexToggles.onAutoResetChange(e.target.checked)}
            />
            Auto-use Codex reset credits (when every account is &gt;95% weekly and none
            reset within 24h, spend one banked reset to bring an account back)
          </label>
        </>
      ) : null}
    </div>
  );
}
