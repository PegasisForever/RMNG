// The clone dialog's group picker plus the two per-side account pickers.
//
// All three are OVERRIDES. Blank means "follow the source", and the blank option says what
// that is, so the operator can see what they are overriding before they override it.
// Picking a pool binds the clone once (both sides draw from it); picking anything else on
// a side pins that side regardless of pool.
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
  groups,
  sourceGroup,
  preset,
  group,
  claudeAccount,
  codexAccount,
  onGroupChange,
  onClaudeAccountChange,
  onCodexAccountChange,
}: {
  /** Both providers' rows, flat and tagged by `provider`, the way `ControlState` carries
   *  them. Each picker takes its own side. */
  accounts: ClaudeUsage[];
  /** The single configured pool list (`config.groups`). */
  groups: CloneGroup[];
  /** The source clone's pool, for the blank label. Null when the source binds none. */
  sourceGroup: string | null;
  /** The preset that will drive the clone, for the two blank labels. Undefined before one
   *  resolves, which is what leaves them reading "Preset default / auto". */
  preset: PresetRedacted | undefined;
  group: string;
  claudeAccount: string;
  codexAccount: string;
  onGroupChange: (value: string) => void;
  onClaudeAccountChange: (value: string) => void;
  onCodexAccountChange: (value: string) => void;
}) {
  return (
    <>
      <label className={`mt-3 ${cloneLabel}`}>
        Group
        <select value={group} onChange={(e) => onGroupChange(e.target.value)} className={cloneField}>
          <option value="">
            {sourceGroup ? `Source default (group:${sourceGroup})` : "Source default / auto"}
          </option>
          <option value="none">Any group (all pools)</option>
          {groups.map((g) => (
            <option key={g.name} value={g.name}>
              {g.name} ({g.accounts.length})
            </option>
          ))}
        </select>
      </label>

      <label className={`mt-3 ${cloneLabel}`}>
        Claude account
        <AccountGroupSelect
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
