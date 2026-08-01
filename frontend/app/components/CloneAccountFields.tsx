// The clone dialog's two account pickers, one per provider.
//
// Both are OVERRIDES. Blank means "follow the resolved preset's default", and the blank
// option says which account or pool that is, so the operator can see what they are overriding
// before they override it. Picking anything else pins the clone to that pool or account
// regardless of preset.
import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { ClaudeUsage } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

/** "Preset default (group:pooled)" when the preset names one, else the generic label. */
function blankLabel(fromPreset: string | undefined): string {
  return fromPreset ? `Preset default (${fromPreset})` : "Preset default / auto";
}

export function CloneAccountFields({
  accounts,
  claudeGroups,
  codexGroups,
  preset,
  claudeAccount,
  codexAccount,
  onClaudeAccountChange,
  onCodexAccountChange,
}: {
  /** Both providers' rows, flat and tagged by `provider`, the way `ControlState` carries
   *  them. Each picker takes its own side. */
  accounts: ClaudeUsage[];
  /** Configured Claude pools (`config.cloneGroups`). */
  claudeGroups: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups: CloneGroup[];
  /** The preset that will drive the clone, for the two blank labels. Undefined before one
   *  resolves, which is what leaves them reading "Preset default / auto". */
  preset: PresetRedacted | undefined;
  claudeAccount: string;
  codexAccount: string;
  onClaudeAccountChange: (value: string) => void;
  onCodexAccountChange: (value: string) => void;
}) {
  return (
    <>
      <label className={`mt-3 ${cloneLabel}`}>
        Claude account
        <AccountGroupSelect
          groups={claudeGroups}
          accounts={accounts.filter((a) => a.provider !== "codex")}
          value={claudeAccount}
          blankLabel={blankLabel(preset?.claudeAccount)}
          onChange={onClaudeAccountChange}
          className={cloneField}
        />
      </label>

      <label className={`mt-3 ${cloneLabel}`}>
        Codex account
        <AccountGroupSelect
          groups={codexGroups}
          accounts={accounts.filter((a) => a.provider === "codex")}
          value={codexAccount}
          blankLabel={blankLabel(preset?.codexAccount)}
          onChange={onCodexAccountChange}
          className={cloneField}
        />
      </label>
    </>
  );
}
