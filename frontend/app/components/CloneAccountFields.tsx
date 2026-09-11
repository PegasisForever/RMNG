// The clone dialog's group picker plus the two per-side account pickers.
//
// The group is the pool the clone draws BOTH providers' accounts from; the two account
// pickers are OVERRIDES. Blank means "follow the source", and the blank option says what
// that is, so the operator can see what they are overriding before they override it.
// Picking a pool binds the clone once (both sides draw from it); picking anything else on
// a side pins that side regardless of pool.
import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { ClaudeUsage } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

/** "Preset default (group:pooled)" when the preset names a pool, "Preset default / auto"
 *  when it names any group. */
export function presetBlankLabel(group: string | undefined): string {
        return group && group !== "none"
                ? `Preset default (group:${group})`
                : "Preset default / auto";
}

export function CloneAccountFields({
        accounts,
        groups,
        sourceGroup,
        groupBlankLabel,
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
        /** The source clone's pool, for the blank label. Null when the source binds none.
         *  Ignored when `groupBlankLabel` is given (the template tab has no source). */
        sourceGroup: string | null;
        /** Blunt override for the group picker's blank option, for tabs with no source
         *  clone to inherit from. Omitted = the source-based label above. */
        groupBlankLabel?: string;
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
                                Account group
                                <select
                                        value={group}
                                        onChange={(e) =>
                                                onGroupChange(e.target.value)
                                        }
                                        className={cloneField}
                                >
                                        <option value="">
                                                {groupBlankLabel ??
                                                        (sourceGroup
                                                                ? `Source default (group:${sourceGroup})`
                                                                : "Source default / auto")}
                                        </option>
                                        <option value="none">
                                                Any group (all pools)
                                        </option>
                                        {groups.map((g) => (
                                                <option
                                                        key={g.name}
                                                        value={g.name}
                                                >
                                                        {g.name} (
                                                        {g.accounts.length})
                                                </option>
                                        ))}
                                </select>
                        </label>

                        <div className="mt-3 flex gap-2">
                                <label className={`w-1/2 ${cloneLabel}`}>
                                        Claude account
                                        <AccountGroupSelect
                                                accounts={accounts.filter(
                                                        (a) =>
                                                                a.provider !==
                                                                "codex",
                                                )}
                                                value={claudeAccount}
                                                blankLabel={presetBlankLabel(
                                                        preset?.group,
                                                )}
                                                onChange={onClaudeAccountChange}
                                                className={cloneField}
                                        />
                                </label>

                                <label className={`w-1/2 ${cloneLabel}`}>
                                        Codex account
                                        <AccountGroupSelect
                                                accounts={accounts.filter(
                                                        (a) =>
                                                                a.provider ===
                                                                "codex",
                                                )}
                                                value={codexAccount}
                                                blankLabel={presetBlankLabel(
                                                        preset?.group,
                                                )}
                                                onChange={onCodexAccountChange}
                                                className={cloneField}
                                        />
                                </label>
                        </div>
                </>
        );
}
