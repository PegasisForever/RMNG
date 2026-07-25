// An account-group picker shared by the clone modal and the per-clone change control.
// Under the group-proxy model a clone binds exactly one pool (a CLIProxyAPI instance)
// or none, so the value is just a group name or the "none" sentinel. CLIProxyAPI owns
// intra-group account selection + failover — the operator only picks the pool.
import type { Group } from "~/lib/wire/Group";

/** Sentinel value for "no inference binding". */
export const NO_GROUP = "none";

export function AccountGroupSelect({
  groups,
  value,
  onChange,
  className,
}: {
  /** Available account groups (from `config.groups`). */
  groups: Group[];
  /** The selected group name, or `NO_GROUP` ("none"). */
  value: string;
  onChange: (value: string) => void;
  className?: string;
}) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)} className={className}>
      <option value={NO_GROUP}>None (no inference)</option>
      {groups.length > 0 ? (
        <optgroup label="Groups">
          {groups.map((g) => (
            <option key={g.name} value={g.name}>
              {g.name}
            </option>
          ))}
        </optgroup>
      ) : null}
    </select>
  );
}
