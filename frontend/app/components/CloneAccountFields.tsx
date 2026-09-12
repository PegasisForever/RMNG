// The clone dialog's group picker plus the two per-side account pickers.
//
// The group is the pool the clone draws BOTH providers' accounts from; the two account
// pickers are OVERRIDES. The container fills the group from the resolved preset until the
// operator touches it by hand. Each side reads "Follow group" until the operator pins an
// account — there is no auto option. Blank group is only the state before a preset
// resolves, or when none is configured: the server then decides.
import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { DropdownSelect } from "~/components/DropdownSelect";
import {
  cloneRow,
  cloneRowField,
  cloneRowLabel,
} from "~/components/cloneFieldStyles";
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
    <div className="space-y-3">
      <label className={cloneRow}>
        <span className={cloneRowLabel}>Account group</span>
        <DropdownSelect
          rows={[
            {
              value: "none",
              label: "Any group (all pools)",
            },
            ...groups.map((g) => ({
              value: g.name,
              label: g.name,
            })),
          ]}
          value={group}
          onChange={onGroupChange}
          label="Account group"
          className={cloneRowField}
        />
      </label>

      <label className={cloneRow}>
        <span className={cloneRowLabel}>Claude account</span>
        <AccountGroupSelect
          accounts={accounts.filter((a) => a.provider !== "codex")}
          value={claudeAccount}
          blankLabel="Follow group"
          showAuto={false}
          flat
          label="Claude account"
          onChange={onClaudeAccountChange}
          className={cloneRowField}
        />
      </label>

      <label className={cloneRow}>
        <span className={cloneRowLabel}>Codex account</span>
        <AccountGroupSelect
          accounts={accounts.filter((a) => a.provider === "codex")}
          value={codexAccount}
          blankLabel="Follow group"
          showAuto={false}
          flat
          label="Codex account"
          onChange={onCodexAccountChange}
          className={cloneRowField}
        />
      </label>
    </div>
  );
}
