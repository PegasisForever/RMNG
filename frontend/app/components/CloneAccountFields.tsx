// The clone dialog's group picker plus the two per-side account pickers.
//
// The group is the pool the clone draws BOTH providers' accounts from; the two account
// pickers are OVERRIDES. The container fills all three from the resolved preset (the
// preset's pool, Auto on both sides) until the operator touches one by hand — there is
// no "preset default" pseudo-option. Blank ("Automatic") is only the state before a
// preset resolves, or when none is configured: the server then decides.
import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { ClaudeUsage } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

export function CloneAccountFields({
        accounts,
        groups,
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
                                        <option value="">Automatic</option>
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
                                                blankLabel="Automatic"
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
                                                blankLabel="Automatic"
                                                onChange={onCodexAccountChange}
                                                className={cloneField}
                                        />
                                </label>
                        </div>
                </>
        );
}
